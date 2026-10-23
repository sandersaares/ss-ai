# Tooling

Applies to: agent workflows.

In general, it is possible to install additional tools in an interactive session if you are missing some tooling for a task.
Ask the user for explicit permission before installing additional development tooling.

# Commits are cheap

Applies to: agent workflows.

When working in a Git repository, commit at meaningful checkpoints. Do not wait for a large amount of changes to accumulate. It is fine to commit work even before it gets reviewed by the user.

# Clean Rust/Cargo projects after commit

Applies to: agent workflows in a Rust/Cargo workspace.

When working in a Rust/Cargo workspace, run a `cargo clean` after creating a commit, to help reduce infinite accumulation of build artifacts.

# History atomicity is not required

Applies to: agent workflows.

There is no strict need to keep each commit or pull request minimal or isolated to one change. A commit can be a completely arbitrary unit of work. A pull request is a unit of review and merge, which may include multiple theoretically isolated changes that just happened to be made together for convenience. That's fine and there is no need to split such a pull request.

# Other people have other preferences

Applies to: agent review workflows.

When reviewing work of others, do not complain about others not following the same instructions and guidelines as you.
Different people may have different preferences and priorities. It is fine for you to make suggestions, raise findings and submit
recommendations to encourage others to operate in the way you feel is best but do not assume that others are following the same
instructions as you or have the same constraints as you.

# Reference session state via absolute path

Applies to: agent workflows.

If talking about files stored in session state, always reference them via absolute path.

# Deferrals should leave a note in the repo

Applies to: agent workflows.

If something is intentionally deferred from scope of the current unit of work, it should be written down and referenced from the repo
for future visibility and tracking. This can be as simple as a `TODO.md` in a project/package/library scope, and/or a work item filed
in a tracker and linked from somewhere in the repo (either the same `TODO.md` or a comment elsewhere). Do not simply ignore deferred
work and pretend someone will do it in the future without it being written down.
