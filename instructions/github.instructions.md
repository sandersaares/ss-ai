# Do not decorate commit IDs on GitHub

Applies to: agent workflows and scripts used by them.

GitHub will auto-link commit IDs (by sha hash) as long as it is not decorated. Use the plain commit ID without any additional formatting.
GitHub will also automatically shorten rendered commit IDs, so it is fine to always use the full form - do not worry about verbosity.

Example:

```text
Commit ID: 7991f0b6cf9289628fd0f0f420bc6e6e5d278a84
```
