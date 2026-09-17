# Security Policy

## Supported versions

Only the latest `0.x` release line receives security fixes. There has been no
`0.1.0` release yet; pre-release `main` is developed in the open and may
change without notice.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Report it through the
private advisory form at
<https://github.com/brian-rey-development/mtpx/security/advisories/new> so it can
be fixed before disclosure.

Please include:

- What you did, step by step, and what you expected vs. what happened.
- The `mtpx` version (`mtpx --version`), OS, and device involved.
- Whether the issue needs a physical device, a malicious file name, or only
  local input to trigger.

This is a one-maintainer project, so expect a first response within 7 days
rather than hours. If the report is confirmed, a fix and a `CHANGELOG.md` entry
under `Security` will ship before any public disclosure, and you will be
credited unless you prefer otherwise.

## Scope notes

`mtpx` talks USB to a physical phone you own and writes only under the local
directory you name. Path parsing rejects `.`, `..`, and absolute segments so
a remote name can never escape the destination root; if you find a bypass,
that is in scope and worth reporting.
