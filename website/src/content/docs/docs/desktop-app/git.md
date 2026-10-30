---
title: Use Git
description: Review, commit, and sync workspace changes with the built-in Git view.
sidebar:
  label: Git
  order: 7
lastUpdated: 2026-10-27
---

:::tip
On macOS, use `Cmd` where this page says `Ctrl`, except `Ctrl+Space`. Click any screenshot to view it full screen.
:::

Git must be installed. The app never asks for passwords, so set up credentials or SSH keys first. Background checks
never pull, merge, or change files.

1. Click the Git badge ① or press `Ctrl+Shift+G`.
1. Click a changed file ② to compare **Last Commit** with **Saved File** ③. Save drafts first.
1. Tick files to commit.
1. Type a **Commit message** ④.
1. Click **Commit selected files** ⑤ and confirm.

![The Git view with the Git badge, a changed file, Last Commit and Saved File, Commit message, Commit selected files, and Comparison Changes numbered.](../../../../assets/docs/desktop/git-changes.png)

**Comparison Changes** ⑥ holds screenshots and results. It starts unselected, and **All** never selects it.

**Discard selected files** restores files and deletes selected new files. It can't be undone.

- "Not tracked": click **Initialize Git**.
- **Fetch** checks the remote now. **Pull** only fast-forwards on a clean tree. **Push** never force-pushes.
- The **Remote** row adds, renames, removes, and edits remotes.
- For conflicts, click **Open in VS Code**.

Every write asks first and can be stopped, but stopping can't undo what the remote accepted.
