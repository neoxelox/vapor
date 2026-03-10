---
name: vapor-debug
description: Debug Vapor app and vapord daemon by analyzing logs, crash reports, and source code to diagnose errors and propose fixes
license: MIT
compatibility: opencode
metadata:
  audience: developers
  platform: macos
---

## What I do

I help you debug issues with the **Vapor** macOS application and its **vapord** sync daemon. I will:

1. Collect and analyze application logs and system crash reports
2. Cross-reference errors against the actual source code
3. Produce a clear diagnostic report with root cause analysis
4. Propose a concrete fix plan and wait for your go-ahead before touching any code

## When to use me

Use this skill when:

- Vapor or vapord has crashed and you want to understand why
- You're seeing unexpected behavior, errors, or sync failures
- You want a systematic review of recent log output
- You need help correlating a crash report back to your source code

## How I work

### Step 1 — Gather logs

Read the application logs and any recent crash reports:

- **App/daemon logs:** `~/.vapor/logs/*.logs` — read all files, paying close attention to timestamps, error levels, stack traces, and any `FATAL`, `ERROR`, or `WARN` entries.
- **Crash reports:** `~/Library/Logs/DiagnosticReports/Vapor*.crash` and `~/Library/Logs/DiagnosticReports/Vapor*.ips` — these are macOS-generated crash reports for the Vapor app or vapord daemon. Parse the exception type, termination reason, faulting thread, and backtrace.

**Time-sensitive:** Logs are append-only and older entries may have been rotated out. Before reading any logs, get the current time in **both formats** — a human-readable string and a numeric Unix timestamp (milliseconds) — since logs use both styles. For example: `date +"%Y-%m-%d %H:%M:%S"` and `date +%s000`. Then focus on log entries within a **±15 minute window** around the current time. If nothing relevant is found in that window, gradually widen it (±1 hour, then ±4 hours). Do not waste time reading ancient log entries that are unlikely to be related to the current issue. The same applies to crash reports — prioritize those with the most recent modification timestamps first.

### Step 2 — Review the source code

Once you have identified the relevant error sites (symbols, function names, file paths, line numbers) from the logs and crash reports:

- Read the corresponding source files in the project.
- Trace the call path that led to the failure.
- Check for obvious issues: nil/force-unwrap crashes, out-of-bounds access, threading problems, file handle leaks, unhandled errors, incorrect state transitions, etc.
- If the crash involves `vapord`, pay special attention to the sync logic, IPC, XPC connections, file coordination, and daemon lifecycle.

### Step 3 — Ask for more context if needed

Do not guess. If you need more information to confidently diagnose the issue, ask the user. Examples of things you might ask:

- "Can you reproduce this crash? If so, what steps trigger it?"
- "Was there a specific user action right before the crash (e.g., clicking sync, opening a file)?"
- "Did this start after a recent code change? If so, which files did you modify?"
- "Is vapord running right now? Can you check with `ps aux | grep vapord` or `launchctl list | grep vapor`?"
- "Can you trigger the issue again and share the new log output?"

Never be afraid to ask — a precise diagnosis is more valuable than a fast but wrong one.

### Step 4 — Write a diagnostic report

Present your findings in this format:

```
## Diagnostic Report

### Summary
One or two sentence description of what went wrong.

### Error Source
- **Component:** Vapor app | vapord daemon | both
- **File(s):** list the relevant source files
- **Log evidence:** quote the key log lines or crash report fields

### Root Cause Analysis
Explain why the crash or error happened. Be specific — reference
exact code paths, line numbers, and the conditions that triggered it.

### Affected Functionality
Describe what user-facing behavior is broken or degraded.

### Fix Plan
Numbered steps describing exactly what code changes you will make
and why each change addresses the root cause.

### Risks & Side Effects
Note anything the fix might affect, any edge cases to watch for,
or any additional testing the user should do.
```

### Step 5 — Wait for confirmation

After presenting the report, **stop and wait** for the user to:

- Approve the fix plan
- Ask questions or request changes to the plan
- Provide additional context that changes the diagnosis

**Do not start writing code until the user explicitly confirms.**

## Important notes

- Always check both Vapor app and vapord logs — issues in one often manifest as errors in the other.
- macOS `.ips` crash reports are JSON-formatted. Parse them to extract the exception type, faulting thread, and symbolicated backtrace.
- macOS `.crash` reports are plain text. Look for the `Exception Type`, `Termination Reason`, and the thread backtraces.
- If logs are very large, summarize the overall health first, then zero in on the errors.
- When reading crash reports, match addresses to symbols using the project's source where possible.
