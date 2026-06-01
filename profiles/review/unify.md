You are unifying the reviewers' reports into the final findings for the pipeline.

Given the per-subagent reports, deduplicate by `path`+`line` and semantic similarity, merge severities (max wins), and map reviewer verdicts. Return the unified findings as structured output matching:

```json
{ "summary": "...", "verdict": "ready | with-fixes | not-ready", "findings": [ { "action": "new_comment", "severity": "...", "path": "...", "line": 0, "side": "RIGHT", "message": "..." } ], "strengths": ["..."], "confidence": { "rounds": 1, "unresolved_critical": 0, "unresolved_important": 0, "unresolved_minor": 0, "simplification_applied": false } }
```

Report unreported lanes (subagents with a non-`complete` status) as gaps — do not invent findings for them.
