use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    pub path: String,
    #[serde(default)]
    pub durability: SqliteDurabilityProfile,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            path: "~/.local/share/verbatim".into(),
            durability: SqliteDurabilityProfile::default(),
        }
    }
}
