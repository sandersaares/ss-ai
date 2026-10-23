# Reviewer registration

## Purpose

Registration records the authenticated account's participation in a review.
It does not approve a pull request or replace the calling skill's permission
and human-input procedures.

Registration follows the utility-wide [design](../design.md). Its command
inputs and result fields are defined in the [package reference](../../README.md).

## Preserving existing participation

GitHub adds only the authenticated login to requested reviewers and preserves
all existing requests. It reports a self-authored pull request without writing.

Azure DevOps adds only the authenticated account with a neutral vote. It never
rewrites an existing reviewer, preserving votes and metadata. Any matching
descriptor, account identifier, or login prevents a destructive duplicate.

The result distinguishes an added reviewer, an account already registered,
and a self-authored GitHub pull request. Teams remain separate identities;
the utility does not infer individual participation from team membership.
