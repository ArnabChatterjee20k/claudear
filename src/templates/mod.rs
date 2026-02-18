//! Custom prompt templates with AGENT.md support.
//!
//! This module provides template loading and rendering for Claude prompts.
//! Templates can come from:
//! 1. The project's AGENT.md file (highest priority)
//! 2. Database-stored templates
//! 3. Built-in default templates

mod loader;
mod renderer;

pub use loader::TemplateLoader;
pub use renderer::{TemplateContext, TemplateRenderer};

/// Default template for issue fixing when no custom template is found.
pub const DEFAULT_FIX_TEMPLATE: &str = r#"You are fixing an issue from {{source}}. Here is the issue context:

{{context}}

Your task:
1. Analyze the issue/error and any stack traces
2. Find the relevant code in this codebase
3. Implement a fix for the issue
4. Write or update tests if applicable
5. Create a PR with your changes
6. Ensure all checks pass on the PR

The PR title should include the issue ID: {{short_id}}

"#;

/// Default template for Linear issues (uses /issue skill).
pub const DEFAULT_LINEAR_TEMPLATE: &str = r#"{{#if has_agent_md}}
{{agent_md}}

---
{{/if}}
Address the following Linear issue:

Issue: {{short_id}} - {{title}}
URL: {{url}}
Priority: {{priority}}

{{#if description}}
Description:
{{description}}
{{/if}}

{{context}}

Create a PR that addresses this issue. Include "{{short_id}}" in the PR title.
"#;

/// Default template for Sentry errors.
pub const DEFAULT_SENTRY_TEMPLATE: &str = r#"{{#if has_agent_md}}
{{agent_md}}

---
{{/if}}
Fix the following error from Sentry:

Error: {{title}}
URL: {{url}}
Event count: {{event_count}}

{{context}}

Analyze the stack trace and error context to identify the root cause.
Implement a fix that prevents this error from occurring.
Write tests to verify the fix if applicable.
Ensure all checks pass on the PR.

Create a PR that fixes this error.
"#;
