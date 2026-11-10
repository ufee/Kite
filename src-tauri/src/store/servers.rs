//! The server list as it lies in `servers.json`. Field names follow the spec and the UI (camelCase).
//! Missing fields take their defaults, so an older or hand-edited file still loads.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub servers: Vec<Server>,
    /// Group order in the sidebar.
    pub groups: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyMode {
    #[default]
    Auto,
    File,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Server {
    /// Stable id: everything internal points to it, so renaming the code keeps all memory.
    pub id: String,
    pub code: String,
    pub group: String,
    pub description: String,
    pub host: String,
    pub port: u16,
    pub users: Vec<ServerUser>,
    pub default_user: String,
    pub key_mode: KeyMode,
    pub key_file: String,
    /// Password-only server; the password itself is in the OS keychain, never here.
    pub auth_pass: bool,
    pub start_dir: String,
    pub keepalive: u32,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            id: String::new(),
            code: String::new(),
            group: String::new(),
            description: String::new(),
            host: String::new(),
            port: 22,
            users: Vec::new(),
            default_user: String::new(),
            key_mode: KeyMode::Auto,
            key_file: String::new(),
            auth_pass: false,
            start_dir: String::new(),
            keepalive: 30,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerUser {
    pub name: String,
    /// `auto` follows the server's key mode; otherwise a key file name from ~/.ssh.
    pub key: String,
}
