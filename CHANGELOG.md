# Changelog

## 1.3.13

- Add user name attribution to messages in shared sessions

## 1.3.12

- Increase default image max size from 2 MB to 10 MB

## 1.3.11

- Update claude-codes to 2.1.49 (String-to-enum migration for subtype, stop_reason, status)
- Re-export `CCSystemSubtype` from shared

## 1.3.10

- Use typed claude-codes structs for task parsing instead of raw JSON field access
- Parse task_type, task_status, and task_usage via typed deserialization in both component logic and renderers

## 1.3.9

- Tasks sidebar: header bar toggles open/close (removed separate X button)
- Tasks sidebar: show running task count in title bar

## 1.3.8

- Add widget protocol specification (`docs/WIDGET_PROTOCOL.md`)

## 1.3.7 and earlier

- See git history for previous changes.
