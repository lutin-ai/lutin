//! Workflow image enumeration + on-demand cdylib byte extraction.
//!
//! Workflows ship as Docker images labelled `lutin.workflow.id=<id>`,
//! with the cdylib bundled inside the image at the path declared by
//! `lutin.workflow.cdylib`. CP doesn't stage the cdylib on the host:
//! when a desktop client asks for it via `GetWorkflowCdylib`, we shell
//! out to docker, read the bytes out of the image, and ship them over
//! the wire. The desktop caches by image digest on its side.
//!
//! Failures here are warnings, not fatal. A CP that can't reach the
//! Docker daemon should still boot and serve everything that doesn't
//! involve workflow images.

use std::io;
use std::process::Command;

const ID_LABEL: &str = "lutin.workflow.id";
const CDYLIB_LABEL: &str = "lutin.workflow.cdylib";
const BUNDLE_LABEL: &str = "lutin.workflow.bundle";
const DISPLAY_NAME_LABEL: &str = "lutin.workflow.display_name";
const ICON_LABEL: &str = "lutin.workflow.icon";

#[derive(Debug, Clone)]
pub struct InstalledWorkflow {
    pub id: String,
    pub image: String,
    /// Docker image id (e.g. `sha256:…`). Stable for the life of the
    /// image; changes when the image is rebuilt. Desktop uses it as the
    /// cdylib cache key.
    pub digest: String,
    /// Falls back to `id` when the image omits the label.
    pub display_name: String,
    /// Falls back to a neutral placeholder when the image omits the
    /// label. Stored as `String` rather than `char` because emoji can
    /// span multiple codepoints (ZWJ sequences).
    pub icon: String,
}

/// Enumerate every workflow image installed on the local docker daemon.
/// Returns one entry per image labelled with `lutin.workflow.id`.
pub fn list_installed() -> Vec<InstalledWorkflow> {
    let images = match list_workflow_images() {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "skipping workflow enumeration (docker unavailable?)");
            return Vec::new();
        }
    };

    images
        .into_iter()
        .filter_map(|image| match inspect_image(&image) {
            Ok(meta) => {
                let display_name = if meta.display_name.is_empty() {
                    meta.id.clone()
                } else {
                    meta.display_name
                };
                let icon = if meta.icon.is_empty() {
                    "🧩".to_owned()
                } else {
                    meta.icon
                };
                Some(InstalledWorkflow {
                    id: meta.id,
                    image,
                    digest: meta.digest,
                    display_name,
                    icon,
                })
            }
            Err(e) => {
                tracing::warn!(image = %image, error = %e, "workflow image inspect failed");
                None
            }
        })
        .collect()
}

/// Read the cdylib bytes out of a workflow image, returning them
/// together with the current image digest. Implemented as
/// `docker run --rm <image> cat <path>` — the entrypoint of every
/// workflow image is the engine binary, so we override it with
/// `/bin/cat`. Stdout becomes the bytes; stderr surfaces any error.
pub fn read_cdylib_bytes(image: &str) -> io::Result<(String, Vec<u8>)> {
    let meta = inspect_image(image)?;
    let path = meta.cdylib_path.as_deref().ok_or_else(|| {
        io::Error::other(format!("{image} missing label {CDYLIB_LABEL}"))
    })?;
    cat_image_path(image, path).map(|bytes| (meta.digest, bytes))
}

/// Read the workflow UI bundle (tar archive) out of an image. Same
/// shape as `read_cdylib_bytes`; the path is declared by the
/// `lutin.workflow.bundle` Docker label.
pub fn read_bundle_bytes(image: &str) -> io::Result<(String, Vec<u8>)> {
    let meta = inspect_image(image)?;
    let path = meta.bundle_path.as_deref().ok_or_else(|| {
        io::Error::other(format!("{image} missing label {BUNDLE_LABEL}"))
    })?;
    cat_image_path(image, path).map(|bytes| (meta.digest, bytes))
}

fn cat_image_path(image: &str, path: &str) -> io::Result<Vec<u8>> {
    let out = Command::new("docker")
        .args(["run", "--rm", "--entrypoint", "/bin/cat", image, path])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "docker run cat {image}:{path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

fn list_workflow_images() -> io::Result<Vec<String>> {
    let out = Command::new("docker")
        .args([
            "image",
            "ls",
            "--filter",
            &format!("label={ID_LABEL}"),
            "--format",
            "{{.Repository}}:{{.Tag}}",
        ])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "docker image ls: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "<none>:<none>")
        .map(str::to_owned)
        .collect())
}

struct ImageMeta {
    id: String,
    /// `None` if the image omits the cdylib label. Required by
    /// `read_cdylib_bytes`; optional during the bundle transition so
    /// images that ship only a bundle still enumerate cleanly.
    cdylib_path: Option<String>,
    /// `None` if the image omits the bundle label. Required by
    /// `read_bundle_bytes`.
    bundle_path: Option<String>,
    digest: String,
    /// Empty if the image omits the label — caller picks a fallback.
    display_name: String,
    /// Empty if the image omits the label — caller picks a fallback.
    icon: String,
}

fn inspect_image(image: &str) -> io::Result<ImageMeta> {
    // `index` returns the empty string for missing keys, so optional
    // labels show up as blank lines rather than aborting the inspect.
    let out = Command::new("docker")
        .args([
            "image",
            "inspect",
            image,
            "--format",
            &format!(
                "{{{{index .Config.Labels \"{ID_LABEL}\"}}}}\n\
                 {{{{index .Config.Labels \"{CDYLIB_LABEL}\"}}}}\n\
                 {{{{index .Config.Labels \"{BUNDLE_LABEL}\"}}}}\n\
                 {{{{.Id}}}}\n\
                 {{{{index .Config.Labels \"{DISPLAY_NAME_LABEL}\"}}}}\n\
                 {{{{index .Config.Labels \"{ICON_LABEL}\"}}}}"
            ),
        ])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "docker image inspect {image}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let id = lines.next().unwrap_or("").trim().to_owned();
    let cdylib_path = lines.next().unwrap_or("").trim().to_owned();
    let bundle_path = lines.next().unwrap_or("").trim().to_owned();
    let digest = lines.next().unwrap_or("").trim().to_owned();
    let display_name = lines.next().unwrap_or("").trim().to_owned();
    let icon = lines.next().unwrap_or("").trim().to_owned();
    if id.is_empty() {
        return Err(io::Error::other(format!(
            "{image} missing required label {ID_LABEL}"
        )));
    }
    if cdylib_path.is_empty() && bundle_path.is_empty() {
        return Err(io::Error::other(format!(
            "{image} missing both {CDYLIB_LABEL} and {BUNDLE_LABEL}"
        )));
    }
    if digest.is_empty() {
        return Err(io::Error::other(format!("{image}: empty image digest")));
    }
    Ok(ImageMeta {
        id,
        cdylib_path: (!cdylib_path.is_empty()).then_some(cdylib_path),
        bundle_path: (!bundle_path.is_empty()).then_some(bundle_path),
        digest,
        display_name,
        icon,
    })
}
