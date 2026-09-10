//! Linux `SecretStore`.
//!
//! Two backends, picked at construction. `VAPOR_SECRETS_COMMAND` names
//! an external secret manager (`pass`, a vault CLI, an `age` wrapper)
//! that is called with `get <name>`, `set <name>` (secret on stdin),
//! `delete <name>`, and `list`; it wins when set, so a headless host
//! keeps its tokens wherever it already keeps secrets. Otherwise a
//! desktop with a session bus and `secret-tool` (libsecret's CLI) uses
//! the Secret Service, filed under the `sh.arn.vapor` service
//! attribute so the keyring UI lists every Vapor item together. A host
//! with neither refuses at construction and names the variable to set;
//! a token is never written to a plain file.

use std::io::Write;
use std::process::{Command, Stdio};

use vapor_shared::constants;

use super::{SecretStore, SecretStoreError};

#[derive(Debug)]
enum Backend {
    /// `VAPOR_SECRETS_COMMAND`.
    Command(String),
    /// libsecret's `secret-tool`, under `service = <namespace>`.
    SecretTool { namespace: String },
}

#[derive(Debug)]
pub struct NativeSecretStore {
    backend: Backend,
}

impl NativeSecretStore {
    pub fn for_current_user() -> Result<Self, SecretStoreError> {
        Self::with_namespace(constants::secrets::STORE_NAMESPACE)
    }

    pub fn with_namespace(namespace: &str) -> Result<Self, SecretStoreError> {
        if let Some(command) = std::env::var_os(constants::env::VAPOR_SECRETS_COMMAND)
            .map(|value| value.to_string_lossy().into_owned())
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(Self {
                backend: Backend::Command(command),
            });
        }
        let has_session_bus =
            std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|value| !value.is_empty());
        if has_session_bus && secret_tool_available() {
            return Ok(Self {
                backend: Backend::SecretTool {
                    namespace: namespace.to_string(),
                },
            });
        }
        Err(SecretStoreError::Unsupported(
            "no secret store on this host: set VAPOR_SECRETS_COMMAND to a program that answers \
             get/set/delete/list (a `pass` or `age` wrapper), or run a desktop session with \
             secret-tool installed",
        ))
    }

    /// The command shim, for tests and for a caller that has the
    /// program in hand.
    pub fn with_command(command: &str) -> Self {
        Self {
            backend: Backend::Command(command.to_string()),
        }
    }

    fn run(
        &self,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<std::process::Output, SecretStoreError> {
        let mut command = match &self.backend {
            Backend::Command(program) => {
                let mut parts = program.split_whitespace();
                let Some(executable) = parts.next() else {
                    return Err(SecretStoreError::Unsupported(
                        "VAPOR_SECRETS_COMMAND is empty",
                    ));
                };
                let mut command = Command::new(executable);
                command.args(parts);
                command
            }
            Backend::SecretTool { .. } => Command::new("secret-tool"),
        };
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| SecretStoreError::Backend(Box::new(error)))?;
        if let Some(text) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            pipe.write_all(text.as_bytes())
                .map_err(|error| SecretStoreError::Backend(Box::new(error)))?;
        }
        child
            .wait_with_output()
            .map_err(|error| SecretStoreError::Backend(Box::new(error)))
    }

    fn failure(args: &[&str], output: &std::process::Output) -> SecretStoreError {
        SecretStoreError::Backend(Box::new(std::io::Error::other(format!(
            "secret command {args:?} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))))
    }
}

fn secret_tool_available() -> bool {
    Command::new("secret-tool")
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

impl SecretStore for NativeSecretStore {
    fn get(&self, name: &str) -> Result<String, SecretStoreError> {
        let output = match &self.backend {
            Backend::Command(_) => self.run(&[constants::secrets::COMMAND_VERB_GET, name], None)?,
            Backend::SecretTool { namespace } => {
                self.run(&["lookup", "service", namespace, "name", name], None)?
            }
        };
        if output.status.code() == Some(1) && output.stdout.is_empty() {
            return Err(SecretStoreError::NotFound(name.to_string()));
        }
        if !output.status.success() {
            return Err(Self::failure(&["get", name], &output));
        }
        let mut value = String::from_utf8_lossy(&output.stdout).into_owned();
        if value.ends_with('\n') {
            value.pop();
        }
        if value.is_empty() {
            return Err(SecretStoreError::NotFound(name.to_string()));
        }
        Ok(value)
    }

    fn set(&self, name: &str, value: &str) -> Result<(), SecretStoreError> {
        let output = match &self.backend {
            Backend::Command(_) => {
                self.run(&[constants::secrets::COMMAND_VERB_SET, name], Some(value))?
            }
            Backend::SecretTool { namespace } => self.run(
                &[
                    "store",
                    "--label",
                    &format!("Vapor {name}"),
                    "service",
                    namespace,
                    "name",
                    name,
                ],
                Some(value),
            )?,
        };
        if output.status.success() {
            Ok(())
        } else {
            Err(Self::failure(&["set", name], &output))
        }
    }

    fn delete(&self, name: &str) -> Result<(), SecretStoreError> {
        let output = match &self.backend {
            Backend::Command(_) => {
                self.run(&[constants::secrets::COMMAND_VERB_DELETE, name], None)?
            }
            Backend::SecretTool { namespace } => {
                self.run(&["clear", "service", namespace, "name", name], None)?
            }
        };
        // Deleting a missing secret is not an error.
        if output.status.success() || output.status.code() == Some(1) {
            Ok(())
        } else {
            Err(Self::failure(&["delete", name], &output))
        }
    }

    fn list(&self) -> Result<Vec<String>, SecretStoreError> {
        let output = match &self.backend {
            Backend::Command(_) => self.run(&[constants::secrets::COMMAND_VERB_LIST], None)?,
            Backend::SecretTool { namespace } => {
                self.run(&["search", "--all", "--unlock", "service", namespace], None)?
            }
        };
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(Self::failure(&["list"], &output));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let names = match &self.backend {
            Backend::Command(_) => text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
            // `secret-tool search` prints one block per item with an
            // `attribute.name = <value>` line.
            Backend::SecretTool { .. } => text
                .lines()
                .filter_map(|line| line.trim().strip_prefix("attribute.name = "))
                .map(ToOwned::to_owned)
                .collect(),
        };
        Ok(names)
    }

    fn is_persistent(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        match &self.backend {
            Backend::Command(program) => format!(
                "external command `{program}` ({})",
                constants::env::VAPOR_SECRETS_COMMAND
            ),
            Backend::SecretTool { .. } => "desktop Secret Service via secret-tool".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell script that keeps secrets as files in a directory: the
    /// smallest program that speaks the shim's verbs.
    fn file_backed_shim(dir: &std::path::Path) -> String {
        let script = dir.join("shim.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nstore=\"{}\"\nmkdir -p \"$store\"\ncase \"$1\" in\n  get) [ -f \"$store/$2\" ] || exit 1; cat \"$store/$2\" ;;\n  set) cat > \"$store/$2\" ;;\n  delete) rm -f \"$store/$2\" ;;\n  list) ls \"$store\" ;;\n  *) exit 2 ;;\nesac\n",
                dir.join("secrets").display()
            ),
        )
        .expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
                .expect("chmod");
        }
        script.display().to_string()
    }

    #[test]
    fn the_command_shim_satisfies_the_contract() {
        let temp = tempfile::TempDir::new().expect("temp");
        let store = NativeSecretStore::with_command(&file_backed_shim(temp.path()));
        assert!(store.is_persistent());
        let a = "contract.alpha";
        let b = "contract.beta";
        assert!(matches!(
            store.get(a).expect_err("missing before set"),
            SecretStoreError::NotFound(_)
        ));
        store.delete(a).expect("deleting a missing secret is fine");
        store.set(a, "one").expect("set a");
        store.set(b, "tökén ✓ {\"json\":true}").expect("set b");
        assert_eq!(store.get(a).expect("get a"), "one");
        assert_eq!(store.get(b).expect("get b"), "tökén ✓ {\"json\":true}");
        store.set(a, "one-updated").expect("overwrite");
        assert_eq!(store.get(a).expect("get"), "one-updated");
        let mut listed = store.list().expect("list");
        listed.sort();
        assert_eq!(listed, vec![a.to_string(), b.to_string()]);
        store.delete(a).expect("delete");
        assert!(matches!(
            store.get(a).expect_err("gone"),
            SecretStoreError::NotFound(_)
        ));
    }

    #[test]
    fn a_host_without_a_backend_names_the_way_out() {
        // With neither the variable nor a session bus the constructor
        // refuses; a test host may have both, so only the message shape
        // is asserted when it does refuse.
        if let Err(error) = NativeSecretStore::with_namespace("sh.arn.vapor.test") {
            assert!(error.to_string().contains("VAPOR_SECRETS_COMMAND"));
        }
    }
}
