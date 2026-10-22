# Provider evidence and identities

## Responsibility

The provider adapter supplies complete, normalized evidence to selection and
registration. It separates provider wire formats from workflow decisions so
both functional areas apply the same coordinate and identity rules.
See the [architecture](../implementation.md) for the surrounding boundaries.

Targets carry explicit provider, organization, repository, and pull request
coordinates. Azure DevOps also carries collection and project coordinates.
Repository targets use the journal's repository URL; candidate targets retain
that scope while adding a positive pull request identifier. Registration can
resolve missing Azure DevOps project and repository coordinates from the
collection and pull request identifier.

## Complete evidence

GitHub REST lists use paginated, slurped results; the adapter validates and
flattens each page. Commit reads use the GraphQL commits connection, not the
capped REST commits endpoint. Azure DevOps open-PR enumeration continues
offset paging through an empty page; commit reads follow continuation tokens.
Other endpoints reject unexpected continuation rather than reporting
incomplete evidence. Repeated identifiers and cursors are errors.

Required arrays and fields have no success-shaped defaults. JSON readers
check shapes at the provider boundary so missing or malformed data cannot
become an empty successful search. Commit recency consumes every commit page
before ranking a candidate.

## Account identity and registration

Azure DevOps connection data identifies the authenticated user, not a different
authorized user. Account descriptors, identifiers, and unique names support
identity comparisons; display names serve only presentation. Ownership uses
the most authoritative shared identifier. Duplicate-registration detection
accepts any matching identifier to protect existing votes and metadata.

GitHub reviewer evidence combines requested users and submitted review
authors without expanding teams or treating pending reviews as participation.
Registration preserves team requests and adds only the authenticated login.
Azure DevOps registration leaves any matching existing reviewer untouched and
creates a neutral reviewer only when none matches.
Selection and confirmation exclude entries with `hasDeclined: true` before
reducing reviewer payloads to identities. Registration includes those entries
so it preserves their votes, decline flags, and other metadata.

Discussion records preserve service creation timestamps and comment/thread
identifiers. Claim ownership requires these identifiers, the author, session,
and token; surrounding announcement prose is not ownership evidence.
