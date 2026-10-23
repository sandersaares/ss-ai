# Parameterized benchmarks should have a low case and a high case

Applies to: microbenchmarks.

Do not create more than two variants of parameterized benchmarks. One "low value" and one "high value" are usually enough to inform us about scaling behavior with different inputs. Any additional variants need separate justification.