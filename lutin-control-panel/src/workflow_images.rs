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

#[derive(Debug, Clone)]
pub struct InstalledWorkflow {
    pub id: String,
    pub image: String,
    /// Docker image id (e.g. `sha256:…`). Stable for the life of the
    /// image; changes when the image is rebuilt. Desktop uses it as the
    /// cdylib cache key.
    pub digest: String,
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
            Ok(meta) => Some(InstalledWorkflow {
                id: meta.id,
                image,
                digest: meta.digest,
            }),
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
    let out = Command::new("docker")
        .args(["run", "--rm", "--entrypoint", "/bin/cat", image, &meta.cdylib_path])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "docker run cat {}:{}: {}",
            image,
            meta.cdylib_path,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok((meta.digest, out.stdout))
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
    cdylib_path: String,
    digest: String,
}

fn inspect_image(image: &str) -> io::Result<ImageMeta> {
    let out = Command::new("docker")
        .args([
            "image",
            "inspect",
            image,
            "--format",
            &format!(
                "{{{{index .Config.Labels \"{ID_LABEL}\"}}}}\n{{{{index .Config.Labels \"{CDYLIB_LABEL}\"}}}}\n{{{{.Id}}}}"
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
    let digest = lines.next().unwrap_or("").trim().to_owned();
    if id.is_empty() {
        return Err(io::Error::other(format!(
            "{image} missing required label {ID_LABEL}"
        )));
    }
    if cdylib_path.is_empty() {
        return Err(io::Error::other(format!(
            "{image} missing required label {CDYLIB_LABEL}"
        )));
    }
    if digest.is_empty() {
        return Err(io::Error::other(format!("{image}: empty image digest")));
    }
    Ok(ImageMeta {
        id,
        cdylib_path,
        digest,
    })
}
