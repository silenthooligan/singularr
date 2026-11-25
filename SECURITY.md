# Security policy

## Supported versions

Singularr is in beta (0.x). The latest 0.x release receives security fixes. Older releases do not.

| Version | Supported |
|---|---|
| Latest 0.x | Yes |
| Older | No |

## Reporting a vulnerability

Report security problems privately through a
[GitHub security advisory](https://github.com/silenthooligan/singularr/security/advisories/new).
Describe the problem and the steps to reproduce it. Do not open a public issue for a security problem.

## Scope

- Use of the workspace or the API without the owner's sign-in or an API key with the right scope
- Identity headers accepted from a peer outside `SINGULARR_TRUSTED_PROXY_IPS` or without the edge token
- A call to an integration that its adapter does not declare
- A declared action sent without write access
- Singularr Requests reading or changing data outside the views made for it
- Secrets (API keys, tokens, passwords) in logs, error messages or API responses
- Remote code execution or file access through data read from an integration

## Deployment

Run the workspace behind a reverse proxy that signs the owner in (README, Sign-in). Only that proxy
should reach the workspace's port. Singularr Requests is the only part meant for viewers.
