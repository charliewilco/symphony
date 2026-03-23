---
tracker:
  kind: linear
  # api_key: $LINEAR_API_KEY  # or set LINEAR_API_KEY env var
  project_slug: YOUR_PROJECT_SLUG
  active_states:
    - Todo
    - In Progress
  terminal_states:
    - Done
    - Cancelled
    - Canceled
    - Duplicate
    - Closed

polling:
  interval_ms: 30000

workspace:
  root: /tmp/symphony_workspaces

agent:
  max_concurrent_agents: 3
  max_turns: 20
  max_retry_backoff_ms: 300000

codex:
  command: codex app-server
  approval_policy: on-failure
  thread_sandbox: none
  turn_timeout_ms: 3600000
  stall_timeout_ms: 300000

hooks:
  timeout_ms: 60000
  # after_create: |
  #   git clone https://github.com/your-org/your-repo.git .
  # before_run: |
  #   git fetch origin && git reset --hard origin/main

# server:
#   port: 8080  # enables the HTTP dashboard at http://127.0.0.1:8080
---

You are an expert software engineer working on a Linear issue.

**Issue:** {{ issue.identifier }}: {{ issue.title }}
**State:** {{ issue.state }}
{% if issue.description %}
**Description:**
{{ issue.description }}
{% endif %}
{% if issue.labels.size > 0 %}
**Labels:** {{ issue.labels | join: ", " }}
{% endif %}
{% if issue.url %}
**Linear URL:** {{ issue.url }}
{% endif %}
{% if attempt %}
**Note:** This is retry/continuation attempt {{ attempt }}. Review any prior work and continue from where you left off.
{% endif %}

## Instructions

1. Understand the issue thoroughly before making changes.
2. Make the minimal necessary changes to complete the issue.
3. Follow the existing code style and conventions.
4. Run tests if available to verify your changes.
5. When complete, update the issue state in Linear to indicate you're done (e.g., move to "Human Review" or "Done").

Work carefully and methodically. Commit your changes with a descriptive commit message.
