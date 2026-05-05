//! Workflow cdylib materialization from Docker images.
//!
//! In the post-refactor model, every workflow ships as a Docker image
//! whose runtime container drives one session. The image also bundles
//! the workflow's UI cdylib (`/workflow/lib.so` by convention) — but
//! the desktop chrome runs on the host and dlopens .so files from the
//! host filesystem, not from inside the container. So at boot we
//! enumerate installed workflow images, `docker create` + `docker cp`
//! the cdylib out of each one, and drop it under
//! `<global>/workflows/<id>/lib<id>.so` where `lutin-desktop`'s loader
//! looks.
//!
//! Idempotent: a `.image_digest` marker next to the cdylib records the
//! image id of the last successful extract; re-running with the same
//! image is a no-op. Re-running after a workflow image rebuild detects
//! the digest mismatch and re-extracts.
//!
//! Failures here are warnings, not fatal. A CP that can't reach the
//! Docker daemon should still boot — operators may be running in
//! `subprocess` mode (legacy lutin-project tier) where these images
//! aren't needed.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::{info, warn};

const ID_LABEL: &str = "lutin.workflow.id";
const CDYLIB_LABEL: &str = "lutin.workflow.cdylib";
const DIGEST_MARKER: &str = ".image_digest";

#[derive(Debug, Clone)]
pub struct InstalledWorkflow {
    pub id: String,
    pub image: String,
    pub so_path: PathBuf,
}

/// Discover installed workflow images and extract their cdylibs. The
/// returned vec lists every workflow whose .so is present on disk
/// after the call (whether freshly extracted or already up-to-date).
pub fn install_all(global_config_dir: &Path) -> Vec<InstalledWorkflow> {
    let images = match list_workflow_images() {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "skipping workflow image install (docker unavailable?)");
            return Vec::new();
        }
    };

    let workflows_root = global_config_dir.join("workflows");
    let mut out = Vec::new();
    for image in images {
        match install_one(&workflows_root, &image) {
            Ok(installed) => out.push(installed),
            Err(e) => warn!(image = %image, error = %e, "workflow image install failed"),
        }
    }
    out
}

/// `docker image ls --filter label=<id-label> --format '{{.Repository}}:{{.Tag}}'`.
/// Filtering on the *presence* of the id label catches every workflow
/// image regardless of repo naming convention.
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
            "docker image ls failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let images: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "<none>:<none>")
        .map(str::to_owned)
        .collect();
    Ok(images)
}

fn install_one(workflows_root: &Path, image: &str) -> io::Result<InstalledWorkflow> {
    let meta = inspect_image(image)?;
    let dst_dir = workflows_root.join(&meta.id);
    let so_path = dst_dir.join(format!("lib{}.so", meta.id));
    let marker_path = dst_dir.join(DIGEST_MARKER);

    if so_path.exists() && std::fs::read_to_string(&marker_path).ok().as_deref() == Some(meta.digest.as_str()) {
        return Ok(InstalledWorkflow {
            id: meta.id,
            image: image.to_owned(),
            so_path,
        });
    }

    info!(image = %image, id = %meta.id, dst = %so_path.display(), "extracting workflow cdylib");
    std::fs::create_dir_all(&dst_dir)?;
    extract_file_from_image(image, &meta.cdylib_path, &so_path)?;
    std::fs::write(&marker_path, &meta.digest)?;
    Ok(InstalledWorkflow {
        id: meta.id,
        image: image.to_owned(),
        so_path,
    })
}

struct ImageMeta {
    id: String,
    cdylib_path: String,
    digest: String,
}

fn inspect_image(image: &str) -> io::Result<ImageMeta> {
    // Single inspect call yielding three lines: id label, cdylib label,
    // image digest. Format-string templating keeps us out of JSON-parse
    // territory.
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

/// `docker create` + `docker cp` + `docker rm`. The created container
/// never starts (so its ENTRYPOINT and required env are irrelevant) —
/// we just need a writable namespace to copy out of. Failures partway
/// through still try to `docker rm` the stub.
fn extract_file_from_image(image: &str, src_in_image: &str, dst: &Path) -> io::Result<()> {
    let create = Command::new("docker")
        .args(["create", image])
        .output()?;
    if !create.status.success() {
        return Err(io::Error::other(format!(
            "docker create {image}: {}",
            String::from_utf8_lossy(&create.stderr).trim()
        )));
    }
    let cid = String::from_utf8_lossy(&create.stdout).trim().to_owned();
    if cid.is_empty() {
        return Err(io::Error::other(format!(
            "docker create {image} produced empty id"
        )));
    }

    let result = (|| -> io::Result<()> {
        let tmp_dst = dst.with_extension("so.tmp");
        if tmp_dst.exists() {
            std::fs::remove_file(&tmp_dst)?;
        }
        let cp = Command::new("docker")
            .args(["cp", &format!("{cid}:{src_in_image}"), &tmp_dst.display().to_string()])
            .output()?;
        if !cp.status.success() {
            return Err(io::Error::other(format!(
                "docker cp {cid}:{src_in_image}: {}",
                String::from_utf8_lossy(&cp.stderr).trim()
            )));
        }
        std::fs::rename(&tmp_dst, dst)?;
        Ok(())
    })();

    // Best-effort cleanup; report the original error if any.
    let _ = Command::new("docker")
        .args(["rm", "-f", &cid])
        .output();
    result
}
