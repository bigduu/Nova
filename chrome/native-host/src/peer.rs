//! Process observations belonging only to one accepted native-host Session.
//! This proves a bounded OS process relationship, not Chrome or window identity.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[cfg(unix)]
use crate::socket::ProcessQuery;
#[cfg(windows)]
use crate::windows::ProcessQuery;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessEvidence {
    pub pid: u32,
    // Opaque platform-local start identity: Darwin sec/usec, Linux ticks/0,
    // Windows creation FILETIME/0. Never compare across machines/platforms.
    pub start: [u64; 2],
    pub image: PathBuf,
    pub parent_pid: u32,
    pub system_cmd: bool,
}

impl ProcessEvidence {
    fn status(&self) -> Value {
        json!({
            "pid": self.pid,
            "startIdentity": self.start,
            "image": self.image.file_name().unwrap_or_default().to_string_lossy(),
            // Keep the full observed executable internally, without returning
            // a directory that may contain a user's private profile path.
            "imagePathSha256": format!("{:x}", Sha256::digest(self.image.to_string_lossy().as_bytes())),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unavailable {
    PeerPid,
    Metadata,
    ParentRelationship,
    UnsupportedLauncher,
    ChangedInstance,
}

impl Unavailable {
    fn reason(self) -> &'static str {
        match self {
            Self::PeerPid => "peer_pid_unavailable",
            Self::Metadata => "process_metadata_unavailable",
            Self::ParentRelationship => "invalid_parent_relationship",
            Self::UnsupportedLauncher => "unsupported_launcher",
            Self::ChangedInstance => "process_instance_changed",
        }
    }
}

struct LaunchChain {
    host: ProcessEvidence,
    parent: ProcessEvidence,
    intermediate: Option<ProcessEvidence>,
}

/// No independent lifetime or recovery: Session owns and drops this witness.
pub(crate) struct ProcessWitness {
    peer_pid: Option<u32>,
    chain: Result<LaunchChain, Unavailable>,
}

impl ProcessWitness {
    pub fn capture(peer_pid: Option<u32>) -> Self {
        let mut query = ProcessQuery::new();
        Self::capture_with(peer_pid, |pid| {
            query
                .as_mut()
                .map_err(|_| Unavailable::Metadata)?
                .get(pid)
                .map_err(|_| Unavailable::Metadata)
        })
    }

    fn capture_with(
        peer_pid: Option<u32>,
        mut get: impl FnMut(u32) -> Result<ProcessEvidence, Unavailable>,
    ) -> Self {
        let chain = (|| {
            let host = get(peer_pid.ok_or(Unavailable::PeerPid)?)?;
            let direct = get(host.parent_pid)?;
            validate_parent(&host, &direct)?;
            let (parent, intermediate) = if direct.system_cmd {
                let parent = get(direct.parent_pid)?;
                validate_parent(&direct, &parent)?;
                if parent.system_cmd {
                    return Err(Unavailable::UnsupportedLauncher);
                }
                (parent, Some(direct))
            } else {
                (direct, None)
            };
            Ok(LaunchChain {
                host,
                parent,
                intermediate,
            })
        })();
        Self { peer_pid, chain }
    }

    pub fn status(&self) -> Value {
        let mut query = ProcessQuery::new();
        self.status_with(|pid| {
            query
                .as_mut()
                .map_err(|_| Unavailable::Metadata)?
                .get(pid)
                .map_err(|_| Unavailable::Metadata)
        })
    }

    fn status_with(
        &self,
        mut get: impl FnMut(u32) -> Result<ProcessEvidence, Unavailable>,
    ) -> Value {
        let live = (|| {
            let chain = self.chain.as_ref().map_err(|reason| *reason)?;
            for expected in std::iter::once(&chain.host)
                .chain(chain.intermediate.iter())
                .chain(std::iter::once(&chain.parent))
            {
                if get(expected.pid)? != *expected {
                    return Err(Unavailable::ChangedInstance);
                }
            }
            Ok(chain)
        })();
        let mut status = json!({
            "kernelPeerPid": self.peer_pid,
            "browserIdentity": "unproven",
            "nativeWindowAssociation": "unproven",
        });
        match live {
            Ok(chain) => {
                status["status"] = json!("process_relationship_observed");
                status["host"] = chain.host.status();
                status["parentCandidate"] = chain.parent.status();
                status["launchKind"] = json!(if chain.intermediate.is_some() {
                    "windows_cmd_parent"
                } else {
                    "direct_parent"
                });
                if let Some(intermediate) = &chain.intermediate {
                    status["intermediate"] = intermediate.status();
                }
            }
            Err(reason) => {
                status["status"] = json!("ownership_unavailable");
                status["reason"] = json!(reason.reason());
            }
        }
        status
    }
}

fn validate_parent(child: &ProcessEvidence, parent: &ProcessEvidence) -> Result<(), Unavailable> {
    if child.parent_pid != parent.pid || child.pid == parent.pid || parent.start > child.start {
        return Err(Unavailable::ParentRelationship);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent_pid: u32) -> ProcessEvidence {
        ProcessEvidence {
            pid,
            parent_pid,
            start: [pid as u64, 0],
            image: format!("/private/{pid}/process").into(),
            system_cmd: false,
        }
    }

    #[test]
    fn direct_observation_has_no_browser_or_window_authority() {
        let witness = ProcessWitness::capture_with(Some(20), |pid| Ok(process(pid, 10)));
        let status = witness.status_with(|pid| Ok(process(pid, 10)));
        assert_eq!(status["launchKind"], "direct_parent");
        assert_eq!(status["parentCandidate"]["pid"], 10);
        assert_eq!(status["browserIdentity"], "unproven");
        assert_eq!(status["nativeWindowAssociation"], "unproven");
        assert!(!status.to_string().contains("/private/"));
    }

    #[test]
    fn changed_pid_start_image_parent_or_missing_process_is_unavailable() {
        let witness = ProcessWitness::capture_with(Some(20), |pid| Ok(process(pid, 10)));
        for field in ["pid", "start", "image", "parent"] {
            let status = witness.status_with(|pid| {
                let mut row = process(pid, 10);
                match field {
                    "pid" => row.pid += 1,
                    "start" => row.start[1] += 1,
                    "image" => row.image = "/replacement".into(),
                    "parent" => row.parent_pid += 1,
                    _ => unreachable!(),
                }
                Ok(row)
            });
            assert_eq!(status["reason"], "process_instance_changed", "{field}");
            assert!(status.get("parentCandidate").is_none());
        }
        assert_eq!(
            witness.status_with(|_| Err(Unavailable::Metadata))["status"],
            "ownership_unavailable"
        );
        let reused = ProcessWitness::capture_with(Some(20), |pid| {
            let mut row = process(pid, 10);
            if pid == 10 {
                row.start = [21, 0];
            }
            Ok(row)
        });
        assert_eq!(
            reused.status_with(|_| unreachable!())["reason"],
            "invalid_parent_relationship"
        );
        let missing = ProcessWitness::capture_with(None, |_| unreachable!());
        assert_eq!(
            missing.status_with(|_| unreachable!())["reason"],
            "peer_pid_unavailable"
        );
    }

    #[test]
    fn one_cmd_hop_is_observed_without_searching_further_ancestors() {
        let get = |pid| {
            let mut row = process(pid, pid - 10);
            row.system_cmd = pid == 20;
            Ok(row)
        };
        let witness = ProcessWitness::capture_with(Some(30), get);
        let status = witness.status_with(get);
        assert_eq!(status["launchKind"], "windows_cmd_parent");
        assert_eq!(status["intermediate"]["pid"], 20);
        assert_eq!(status["parentCandidate"]["pid"], 10);
        let unsupported = ProcessWitness::capture_with(Some(30), |pid| {
            let mut row = process(pid, pid - 10);
            row.system_cmd = pid != 30;
            Ok(row)
        });
        assert_eq!(
            unsupported.status_with(|_| unreachable!())["reason"],
            "unsupported_launcher"
        );
    }
}
