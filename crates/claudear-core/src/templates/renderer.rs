//! Template rendering with variable substitution.

use crate::types::Issue;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Pre-compiled regex for `{{#each array}}...{{/each}}` loops.
static EACH_REGEX: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
    regex_lite::Regex::new(r"\{\{#each\s+(\w+)\}\}([\s\S]*?)\{\{/each\}\}").unwrap()
});

/// Pre-compiled regex for `{{#if field}}...{{/if}}` conditionals.
static IF_REGEX: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"\{\{#if\s+(\w+)\}\}([\s\S]*?)\{\{/if\}\}").unwrap());

/// Context for template rendering.
#[derive(Debug, Clone)]
pub struct TemplateContext {
    /// The issue being fixed.
    pub issue: Issue,
    /// Additional context from the source (stack traces, etc.).
    pub source_context: String,
    /// Content from AGENT.md if present.
    pub agent_md: Option<String>,
    /// Additional custom variables.
    pub variables: HashMap<String, String>,
    /// Array variables for loop support.
    pub arrays: HashMap<String, Vec<HashMap<String, String>>>,
}

impl TemplateContext {
    /// Create a new template context for an issue.
    pub fn new(issue: Issue, source_context: String) -> Self {
        Self {
            issue,
            source_context,
            agent_md: None,
            variables: HashMap::new(),
            arrays: HashMap::new(),
        }
    }

    /// Set the AGENT.md content.
    pub fn with_agent_md(mut self, content: Option<String>) -> Self {
        self.agent_md = content;
        self
    }

    /// Add a custom variable.
    pub fn with_variable(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.variables.insert(key.into(), value.into());
        self
    }

    /// Add an array variable for loop iteration.
    pub fn with_array(
        mut self,
        key: impl Into<String>,
        items: Vec<HashMap<String, String>>,
    ) -> Self {
        self.arrays.insert(key.into(), items);
        self
    }

    /// Add an array from JSON value.
    pub fn with_json_array(mut self, key: impl Into<String>, value: &JsonValue) -> Self {
        if let Some(arr) = value.as_array() {
            let items: Vec<HashMap<String, String>> = arr
                .iter()
                .filter_map(|item| {
                    if let Some(obj) = item.as_object() {
                        let map: HashMap<String, String> = obj
                            .iter()
                            .map(|(k, v)| {
                                let value_str = match v {
                                    JsonValue::String(s) => s.clone(),
                                    JsonValue::Number(n) => n.to_string(),
                                    JsonValue::Bool(b) => b.to_string(),
                                    _ => v.to_string(),
                                };
                                (k.clone(), value_str)
                            })
                            .collect();
                        Some(map)
                    } else if let Some(s) = item.as_str() {
                        // Simple string array - use "item" as the key
                        let mut map = HashMap::new();
                        map.insert("item".to_string(), s.to_string());
                        Some(map)
                    } else {
                        None
                    }
                })
                .collect();
            self.arrays.insert(key.into(), items);
        }
        self
    }
}

/// Renders templates with variable substitution.
pub struct TemplateRenderer;

impl TemplateRenderer {
    /// Create a new template renderer.
    pub fn new() -> Self {
        Self
    }

    /// Render a template with the given context.
    /// Uses simple {{variable}} substitution.
    pub fn render(&self, template: &str, context: &TemplateContext) -> String {
        let mut result = template.to_string();

        // Process loops first (before variable substitution)
        result = self.process_loops(&result, context);

        // Issue fields
        result = result.replace("{{id}}", &context.issue.id);
        result = result.replace("{{short_id}}", &context.issue.short_id);
        result = result.replace("{{title}}", &context.issue.title);
        result = result.replace("{{url}}", &context.issue.url);
        result = result.replace("{{source}}", &context.issue.source);
        result = result.replace("{{priority}}", &context.issue.priority.to_string());
        result = result.replace("{{status}}", &context.issue.status.to_string());

        // Description with fallback
        let description = context.issue.description.as_deref().unwrap_or("");
        result = result.replace("{{description}}", description);

        // Source context
        result = result.replace("{{context}}", &context.source_context);

        // AGENT.md content
        let has_agent_md = context.agent_md.is_some();
        result = result.replace(
            "{{has_agent_md}}",
            if has_agent_md { "true" } else { "false" },
        );
        result = result.replace("{{agent_md}}", context.agent_md.as_deref().unwrap_or(""));

        // Metadata fields
        if let Some(event_count) = context.issue.get_metadata::<u64>("event_count") {
            result = result.replace("{{event_count}}", &event_count.to_string());
        } else {
            result = result.replace("{{event_count}}", "N/A");
        }

        // Custom variables
        for (key, value) in &context.variables {
            result = result.replace(&format!("{{{{{}}}}}", key), value);
        }

        // Handle simple conditionals: {{#if field}}content{{/if}}
        result = self.process_conditionals(&result, context);

        result
    }

    /// Process {{#each array}}...{{/each}} loops.
    fn process_loops(&self, template: &str, context: &TemplateContext) -> String {
        let mut result = template.to_string();

        // Process {{#each array}}...{{/each}} blocks
        // Supports accessing item properties via {{property}} or {{this.property}}
        // For simple arrays, use {{item}} or {{this}}
        result = EACH_REGEX
            .replace_all(&result, |caps: &regex_lite::Captures| {
                let array_name = &caps[1];
                let loop_body = &caps[2];

                if let Some(items) = context.arrays.get(array_name) {
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| {
                            let mut iteration = loop_body.to_string();

                            // Replace {{@index}} with the current index
                            iteration = iteration.replace("{{@index}}", &index.to_string());

                            // Replace {{@first}} with true/false
                            iteration = iteration
                                .replace("{{@first}}", if index == 0 { "true" } else { "false" });

                            // Replace {{@last}} with true/false
                            iteration = iteration.replace(
                                "{{@last}}",
                                if index == items.len() - 1 {
                                    "true"
                                } else {
                                    "false"
                                },
                            );

                            // Replace {{this}} with the item value (for simple string arrays)
                            if let Some(value) = item.get("item") {
                                iteration = iteration.replace("{{this}}", value);
                            }

                            // Replace {{property}} and {{this.property}} with item values
                            for (key, value) in item {
                                iteration = iteration.replace(&format!("{{{{{}}}}}", key), value);
                                iteration =
                                    iteration.replace(&format!("{{{{this.{}}}}}", key), value);
                            }

                            iteration
                        })
                        .collect::<Vec<_>>()
                        .join("")
                } else {
                    // Array not found, return empty string
                    String::new()
                }
            })
            .to_string();

        result
    }

    /// Process simple if/endif conditionals.
    fn process_conditionals(&self, template: &str, context: &TemplateContext) -> String {
        let mut result = template.to_string();

        // Process {{#if field}}...{{/if}} blocks
        result = IF_REGEX
            .replace_all(&result, |caps: &regex_lite::Captures| {
                let field = &caps[1];
                let content = &caps[2];

                let should_include = match field {
                    "has_agent_md" => context.agent_md.is_some(),
                    "description" => context.issue.description.is_some(),
                    // Check if array exists and is not empty
                    _ if context.arrays.contains_key(field) => context
                        .arrays
                        .get(field)
                        .map(|a| !a.is_empty())
                        .unwrap_or(false),
                    _ => context
                        .variables
                        .get(field)
                        .map(|v| !v.is_empty())
                        .unwrap_or(false),
                };

                if should_include {
                    content.to_string()
                } else {
                    String::new()
                }
            })
            .to_string();

        result
    }
}

impl Default for TemplateRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_issue() -> Issue {
        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix the bug",
            "https://example.com/issue/123",
            "linear",
        );
        issue.description = Some("This is a bug description".to_string());
        issue
    }

    #[test]
    fn test_basic_variable_substitution() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(create_test_issue(), "Stack trace here".to_string());

        let template = "Fix {{short_id}}: {{title}}";
        let result = renderer.render(template, &context);

        assert_eq!(result, "Fix PROJ-123: Fix the bug");
    }

    #[test]
    fn test_context_substitution() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(create_test_issue(), "Error at line 42".to_string());

        let template = "Context:\n{{context}}";
        let result = renderer.render(template, &context);

        assert_eq!(result, "Context:\nError at line 42");
    }

    #[test]
    fn test_agent_md_substitution() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(create_test_issue(), "".to_string())
            .with_agent_md(Some("Custom instructions".to_string()));

        let template = "{{#if has_agent_md}}{{agent_md}}\n---\n{{/if}}Fix the issue";
        let result = renderer.render(template, &context);

        assert!(result.contains("Custom instructions"));
        assert!(result.contains("---"));
    }

    #[test]
    fn test_conditional_without_agent_md() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(create_test_issue(), "".to_string());

        let template = "{{#if has_agent_md}}AGENT.md present\n{{/if}}Main content";
        let result = renderer.render(template, &context);

        assert!(!result.contains("AGENT.md present"));
        assert!(result.contains("Main content"));
    }

    #[test]
    fn test_conditional_with_description() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(create_test_issue(), "".to_string());

        let template = "{{#if description}}Description: {{description}}{{/if}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("Description: This is a bug description"));
    }

    #[test]
    fn test_custom_variables() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(create_test_issue(), "".to_string())
            .with_variable("branch", "fix/bug-123");

        let template = "Branch: {{branch}}";
        let result = renderer.render(template, &context);

        assert_eq!(result, "Branch: fix/bug-123");
    }

    #[test]
    fn test_full_template() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(
            create_test_issue(),
            "Error: NullPointerException".to_string(),
        )
        .with_agent_md(Some("Follow coding standards".to_string()));

        let template = r#"{{#if has_agent_md}}
{{agent_md}}

---
{{/if}}
Fix issue {{short_id}}: {{title}}
Source: {{source}}
URL: {{url}}

{{#if description}}
Description:
{{description}}
{{/if}}

Context:
{{context}}"#;

        let result = renderer.render(template, &context);

        assert!(result.contains("Follow coding standards"));
        assert!(result.contains("Fix issue PROJ-123: Fix the bug"));
        assert!(result.contains("Source: linear"));
        assert!(result.contains("Description:"));
        assert!(result.contains("This is a bug description"));
        assert!(result.contains("Error: NullPointerException"));
    }

    #[test]
    fn test_each_basic_array() {
        let renderer = TemplateRenderer::new();
        let items = vec![
            HashMap::from([
                ("name".to_string(), "Alice".to_string()),
                ("role".to_string(), "Developer".to_string()),
            ]),
            HashMap::from([
                ("name".to_string(), "Bob".to_string()),
                ("role".to_string(), "Designer".to_string()),
            ]),
        ];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("users", items);

        let template = "Users:{{#each users}}\n- {{name}} ({{role}}){{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("- Alice (Developer)"));
        assert!(result.contains("- Bob (Designer)"));
    }

    #[test]
    fn test_each_with_this_property() {
        let renderer = TemplateRenderer::new();
        let items = vec![
            HashMap::from([("name".to_string(), "Item1".to_string())]),
            HashMap::from([("name".to_string(), "Item2".to_string())]),
        ];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", items);

        let template = "{{#each items}}{{this.name}} {{/each}}";
        let result = renderer.render(template, &context);

        assert_eq!(result, "Item1 Item2 ");
    }

    #[test]
    fn test_each_with_index() {
        let renderer = TemplateRenderer::new();
        let items = vec![
            HashMap::from([("name".to_string(), "First".to_string())]),
            HashMap::from([("name".to_string(), "Second".to_string())]),
            HashMap::from([("name".to_string(), "Third".to_string())]),
        ];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", items);

        let template = "{{#each items}}{{@index}}: {{name}}\n{{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("0: First"));
        assert!(result.contains("1: Second"));
        assert!(result.contains("2: Third"));
    }

    #[test]
    fn test_each_with_first_last() {
        let renderer = TemplateRenderer::new();
        let items = vec![
            HashMap::from([("name".to_string(), "A".to_string())]),
            HashMap::from([("name".to_string(), "B".to_string())]),
            HashMap::from([("name".to_string(), "C".to_string())]),
        ];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", items);

        let template = "{{#each items}}{{name}}(first={{@first}},last={{@last}}) {{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("A(first=true,last=false)"));
        assert!(result.contains("B(first=false,last=false)"));
        assert!(result.contains("C(first=false,last=true)"));
    }

    #[test]
    fn test_each_empty_array() {
        let renderer = TemplateRenderer::new();
        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", vec![]);

        let template = "Before{{#each items}}\n- {{name}}{{/each}}After";
        let result = renderer.render(template, &context);

        assert_eq!(result, "BeforeAfter");
    }

    #[test]
    fn test_each_nonexistent_array() {
        let renderer = TemplateRenderer::new();
        let context = TemplateContext::new(create_test_issue(), "".to_string());

        let template = "Before{{#each missing}}ITEM{{/each}}After";
        let result = renderer.render(template, &context);

        assert_eq!(result, "BeforeAfter");
    }

    #[test]
    fn test_each_simple_string_array() {
        let renderer = TemplateRenderer::new();
        let items = vec![
            HashMap::from([("item".to_string(), "apple".to_string())]),
            HashMap::from([("item".to_string(), "banana".to_string())]),
            HashMap::from([("item".to_string(), "cherry".to_string())]),
        ];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("fruits", items);

        let template = "Fruits: {{#each fruits}}{{this}}, {{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("apple"));
        assert!(result.contains("banana"));
        assert!(result.contains("cherry"));
    }

    #[test]
    fn test_each_with_json_array() {
        let renderer = TemplateRenderer::new();
        let json: serde_json::Value = serde_json::json!([
            {"file": "src/main.rs", "line": 42},
            {"file": "src/lib.rs", "line": 100}
        ]);

        let context = TemplateContext::new(create_test_issue(), "".to_string())
            .with_json_array("stack_frames", &json);

        let template = "Stack:{{#each stack_frames}}\n  {{file}}:{{line}}{{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("src/main.rs:42"));
        assert!(result.contains("src/lib.rs:100"));
    }

    #[test]
    fn test_each_with_json_string_array() {
        let renderer = TemplateRenderer::new();
        let json: serde_json::Value = serde_json::json!(["tag1", "tag2", "tag3"]);

        let context = TemplateContext::new(create_test_issue(), "".to_string())
            .with_json_array("tags", &json);

        let template = "Tags: {{#each tags}}#{{this}} {{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("#tag1"));
        assert!(result.contains("#tag2"));
        assert!(result.contains("#tag3"));
    }

    #[test]
    fn test_each_multiple_loops() {
        let renderer = TemplateRenderer::new();
        let users = vec![
            HashMap::from([("name".to_string(), "Alice".to_string())]),
            HashMap::from([("name".to_string(), "Bob".to_string())]),
        ];
        let tasks = vec![
            HashMap::from([("task".to_string(), "Fix bug".to_string())]),
            HashMap::from([("task".to_string(), "Add feature".to_string())]),
        ];

        let context = TemplateContext::new(create_test_issue(), "".to_string())
            .with_array("users", users)
            .with_array("tasks", tasks);

        let template =
            "Users:{{#each users}} {{name}}{{/each}}\nTasks:{{#each tasks}} {{task}}{{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("Users: Alice Bob"));
        assert!(result.contains("Tasks: Fix bug Add feature"));
    }

    #[test]
    fn test_each_with_special_characters() {
        let renderer = TemplateRenderer::new();
        let items = vec![
            HashMap::from([("text".to_string(), "Hello <World>".to_string())]),
            HashMap::from([("text".to_string(), "Test & Debug".to_string())]),
        ];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", items);

        let template = "{{#each items}}{{text}}\n{{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("Hello <World>"));
        assert!(result.contains("Test & Debug"));
    }

    #[test]
    fn test_if_array_exists() {
        let renderer = TemplateRenderer::new();
        let items = vec![HashMap::from([("name".to_string(), "Item".to_string())])];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", items);

        let template = "{{#if items}}Has items{{/if}}";
        let result = renderer.render(template, &context);

        assert_eq!(result, "Has items");
    }

    #[test]
    fn test_if_empty_array_not_shown() {
        let renderer = TemplateRenderer::new();
        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", vec![]);

        let template = "{{#if items}}Has items{{/if}}Empty";
        let result = renderer.render(template, &context);

        assert_eq!(result, "Empty");
    }

    #[test]
    fn test_each_with_multiline_content() {
        let renderer = TemplateRenderer::new();
        let items = vec![
            HashMap::from([
                ("title".to_string(), "Issue 1".to_string()),
                ("desc".to_string(), "Description 1".to_string()),
            ]),
            HashMap::from([
                ("title".to_string(), "Issue 2".to_string()),
                ("desc".to_string(), "Description 2".to_string()),
            ]),
        ];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("issues", items);

        let template = r#"Issues:
{{#each issues}}
## {{title}}
{{desc}}

{{/each}}"#;
        let result = renderer.render(template, &context);

        assert!(result.contains("## Issue 1\nDescription 1"));
        assert!(result.contains("## Issue 2\nDescription 2"));
    }

    #[test]
    fn test_each_single_item() {
        let renderer = TemplateRenderer::new();
        let items = vec![HashMap::from([("name".to_string(), "Only".to_string())])];

        let context =
            TemplateContext::new(create_test_issue(), "".to_string()).with_array("items", items);

        let template = "{{#each items}}{{@first}}-{{@last}}-{{name}}{{/each}}";
        let result = renderer.render(template, &context);

        // Single item should be both first and last
        assert_eq!(result, "true-true-Only");
    }

    #[test]
    fn test_each_combined_with_if() {
        let renderer = TemplateRenderer::new();
        let items = vec![HashMap::from([("name".to_string(), "Test".to_string())])];

        let context = TemplateContext::new(create_test_issue(), "".to_string())
            .with_array("items", items)
            .with_agent_md(Some("Guidelines".to_string()));

        let template = r#"{{#if has_agent_md}}{{agent_md}}{{/if}}
Items:{{#each items}} {{name}}{{/each}}"#;
        let result = renderer.render(template, &context);

        assert!(result.contains("Guidelines"));
        assert!(result.contains("Items: Test"));
    }

    #[test]
    fn test_each_with_numeric_values() {
        let renderer = TemplateRenderer::new();
        let json: serde_json::Value = serde_json::json!([
            {"count": 10, "name": "errors"},
            {"count": 5, "name": "warnings"}
        ]);

        let context = TemplateContext::new(create_test_issue(), "".to_string())
            .with_json_array("metrics", &json);

        let template = "{{#each metrics}}{{name}}: {{count}}\n{{/each}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("errors: 10"));
        assert!(result.contains("warnings: 5"));
    }

    #[test]
    fn test_each_preserves_surrounding_template() {
        let renderer = TemplateRenderer::new();
        let items = vec![HashMap::from([("x".to_string(), "A".to_string())])];

        let context =
            TemplateContext::new(create_test_issue(), "ctx".to_string()).with_array("items", items);

        let template = "Issue: {{short_id}}\n{{#each items}}{{x}}{{/each}}\nContext: {{context}}";
        let result = renderer.render(template, &context);

        assert!(result.contains("Issue: PROJ-123"));
        assert!(result.contains("A"));
        assert!(result.contains("Context: ctx"));
    }
}
