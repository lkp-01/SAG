# TEMP TODO Tracker (Execution Order)

Last updated: 2026-04-14

Purpose: avoid context loss during long multi-todo implementation.

## Mandatory execution rules

1. Start only one todo as `IN_PROGRESS` at a time.
2. After each todo reaches a runnable state:
   - run checks (`cargo check` / frontend lint as applicable),
   - `git add` + `git commit`,
   - `git push` immediately.
3. After each push, update:
   - `README.md` (new entry points / API / deploy notes),
   - `Context_Handoff.md` (session delta, commit id, deploy sync note).
4. Linux deployment must follow every pushed todo:
   - `git pull`
   - `docker compose up -d --build --force-recreate <affected services>`

## Todo queue

1. [DONE] `admin-app-route-openapi`
   - Apps primary data + API route CRUD + OpenAPI importer
   - Pushed commit: `eca0a373`

2. [DONE] `admin-identity-mapping-ui`
   - Identity provider page + group-role mapping page
   - Pushed commit: `35f61fd6`

3. [DONE] `sag-auth-oidc-standardize`
   - Extend sag-auth from fixed 4A flow to configurable OIDC/4A code flow
   - Parse `groups` from ID token / userinfo and carry into downstream mapping
   - Pushed commit: `7c0cecde`

4. [DONE] `policy-role-mapping-service`
   - Role mapping model/API in sag-policy or shared storage
   - Pushed commit: `e7e945c1`

5. [DONE] `portal-permission-test-loop`
   - Portal permission list + test access actions
   - Pushed commit: `9274dc75`

6. [DONE] `audit-center-and-collector`
   - JSON audit log + collector + admin query view
   - Pushed commit: `5b8dd4a4`

7. [DONE] `observability-entry`
   - Unified observability entry in admin console
   - Pushed commit: `TBD`
