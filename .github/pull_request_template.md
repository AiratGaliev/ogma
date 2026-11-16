## Goal

<!-- What problem is this change solving? Link the Linear issue and the relevant architecture requirement. -->

## Scope

<!-- Summarize the coherent change and list intentionally excluded work. -->

## Branch route

- [ ] Topic branch → dev, dev → test, or test → main follows the documented route.
- [ ] This PR references the correct OGM issue and does not mix unrelated tasks.
- [ ] A promotion PR represents a complete candidate, not a partial queue of unfinished changes.
- [ ] Any exceptional hotfix route explains the urgency, test impact and backport plan.

## Verification

| Check | Result | Evidence |
|---|---|---|
| Focused tests / proof harness | <!-- PASS / FAIL / NOT RUN / N/A --> | |
| Formatting / static checks / compilation | <!-- PASS / FAIL / NOT RUN / N/A --> | |
| Fast CI | <!-- PASS / FAIL / PENDING / N/A --> | |
| Conformance promotion gate (PR to test/main only) | <!-- PASS / FAIL / PENDING / BLOCKED --> | |
| Benchmark / resource measurements | <!-- PASS / FAIL / NOT RUN / N/A --> | |

**Exact commands and results:**

<!-- Include the command, exit status and useful evidence. Distinguish local results from GitHub CI. -->

**Not run / skipped:**

<!-- Explain checks not run and why. Never mark an unexecuted check as passed. -->

## Documentation, compatibility and risks

- [ ] Relevant documentation and ADRs are updated, or no documentation change is needed.
- [ ] Public contracts, compatibility profile, ABI/IR/bytecode format and fallback behaviour are addressed where affected.
- [ ] No unrelated files, build output, secrets or unexplained vendored content are included.
- [ ] Known limitations, residual risks and follow-up issues are listed.

## Acceptance

- [ ] The issue's acceptance criteria are satisfied and evidence is recorded.
- [ ] The change received the appropriate review.
- [ ] No automatic merge or required-check bypass is used.
- [ ] For promotion/release PRs, the candidate SHA and full-gate evidence are identified.
