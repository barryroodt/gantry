---
name: branch-prune
description: This is a skill that should be used by the assistant whenever the user wants to or is asking to clean up, prune, tidy, remove, or otherwise get rid of old, stale, dead, merged, or no-longer-needed local git branches from their repository checkout, especially after pull requests have been merged. Triggers on "prune branches", "clean up branches", "delete merged branches", "tidy branches", "remove old branches", "branch cleanup".
---

# Branch Prune

This skill helps you clean up local git branches that are no longer needed. It is an end-to-end workflow that finds branches which have already been merged into the main branch and removes them so that the local checkout stays tidy and easy to navigate over time.

## Workflow

```dot
digraph branch_prune {
  rankdir=TB;
  "Start" [shape=doublecircle];
  "Fetch" [shape=box, label="1. Fetch + prune remotes"];
  "List" [shape=box, label="2. List merged branches"];
  "Any merged?" [shape=diamond];
  "Confirm" [shape=box, label="3. Confirm with user"];
  "Delete" [shape=box, label="4. Delete merged branches"];
  "Report" [shape=box, label="5. Report what was deleted"];
  "Done" [shape=doublecircle];

  "Start" -> "Fetch";
  "Fetch" -> "List";
  "List" -> "Any merged?";
  "Any merged?" -> "Confirm" [label="yes"];
  "Any merged?" -> "Report" [label="no"];
  "Confirm" -> "Delete";
  "Delete" -> "Report";
  "Report" -> "Done";
}
```

## Steps

### 1. Fetch and prune remotes

```bash
git fetch --all --prune
```

### 2. List branches already merged into main

```bash
git branch --merged main | grep -v -E '^\*|main'
```

### 3. Confirm with the user

Show the list of merged branches and ask the user to confirm before deleting anything. Never delete branches without confirmation.

### 4. Delete the merged branches

```bash
git branch --merged main | grep -v -E '^\*|main' | xargs -r git branch -d
```

### 5. Report

Print a short summary of which branches were deleted and which (if any) were skipped.
