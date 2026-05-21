WORKAROUND for an upstream waza/copilot-cli bug (tracked at
microsoft/waza; see copilot-sdk#947): do not invoke the `task` tool to
spawn sub-agents in this run. Sub-agent tool calls currently hang
indefinitely because the bundled copilot-cli 1.0.2 in waza 0.31.0
predates the sub-agent permission fix (shipped in copilot-cli 1.0.49).
Read and edit files directly with `view` / `str_replace` / `create`.
Once waza ships a release with copilot-cli >= 1.0.49 this constraint
should be removed.
