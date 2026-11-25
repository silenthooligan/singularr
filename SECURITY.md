# Security Policy

## Supported Versions

Singularr is in beta (0.x). The latest 0.x release receives security fixes; older releases do not.

| Version | Supported |
|---|---|
| Latest 0.x | Yes |
| Anything older | No |

## Reporting a Vulnerability

Please report security issues privately through GitHub Security Advisories:

1. Open the [private vulnerability report](https://github.com/silenthooligan/singularr/security/advisories/new).
2. Describe the issue and the steps to reproduce it.

Please do not file public issues for security problems.

## What counts as a security issue

- Using the workspace or the API without being the owner or holding a key with the right scope
- Identity headers honored from a peer outside `SINGULARR_TRUSTED_PROXY_IPS`, or without the edge token
- A call to an integration that its adapter does not declare, or a declared act sent without write access
- Singularr Requests reading or changing anything beyond the views made for it
- Secrets (API keys, tokens, passwords) in logs, error messages or API answers
- Remote code execution or file access through data read from an integration

## How Singularr is meant to be deployed

Behind a reverse proxy that signs the owner in (README, "Sign-in"), with the workspace's port reachable
only by that proxy. Singularr Requests, the viewer portal, is the part meant to face viewers.
