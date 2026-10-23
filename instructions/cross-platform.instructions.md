# Do not infer path case sensitivity from the operating system

Applies to: code files.

Path case sensitivity can depend on the filesystem, volume or mount configuration, and per-directory settings. Do not infer it from the operating system alone; probe actual behavior when correctness or tests depend on it.
