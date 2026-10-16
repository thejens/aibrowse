use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One node of `Accessibility.getFullAXTree`, deserialized leniently: Chrome adds roles
/// and properties every release, so everything is a string or raw JSON here.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AxNode {
    pub node_id: String,
    pub ignored: bool,
    pub role: Option<AxValue>,
    pub name: Option<AxValue>,
    pub description: Option<AxValue>,
    pub value: Option<AxValue>,
    pub properties: Vec<AxProperty>,
    pub parent_id: Option<String>,
    pub child_ids: Vec<String>,
    #[serde(rename = "backendDOMNodeId")]
    pub backend_dom_node_id: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AxValue {
    pub value: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AxProperty {
    pub name: String,
    pub value: AxValue,
}

impl AxNode {
    pub fn role(&self) -> &str {
        self.role
            .as_ref()
            .and_then(|r| r.value.as_str())
            .unwrap_or("")
    }

    pub fn name(&self) -> &str {
        self.name
            .as_ref()
            .and_then(|n| n.value.as_str())
            .unwrap_or("")
    }

    pub fn description(&self) -> &str {
        self.description
            .as_ref()
            .and_then(|d| d.value.as_str())
            .unwrap_or("")
    }

    pub fn value_text(&self) -> Option<String> {
        let value = &self.value.as_ref()?.value;
        match value {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }

    pub fn property(&self, name: &str) -> Option<&Value> {
        self.properties
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.value.value)
    }
}
