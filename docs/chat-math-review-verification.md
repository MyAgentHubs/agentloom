# Chat math and path-copy review verification

Review base: `cd7c02a92c4e33b5ce89e4969d22e32e94630a8f`  
Reviewed head: `578d43a1a9018a5bb158c878f5b43dee71f9166d`  
Tested local source: `875545a2149832567d64be0119a7b4d7bc6b7758`  
Runtime: Linux, Node `v24.19.0`. The branch was still at the reviewed head when this repair started.

Published source tree: `541a813e9cae76e43533c7ac45483b25096f9247`, identical to the tested local source tree. Publishing used the GitHub connector because CLI Git had no GitHub credentials; commit metadata differs, file contents do not.

## Review disposition

All eight reports were accepted. No counter-evidence is claimed. Item 4 has a CSS repair but its real-window acceptance remains pending.

| # | Change | Source | Regression evidence |
|---|---|---|---|
| 1 | KaTeX now has `maxSize: 20` (em), retaining `trust: false` and `maxExpand: 1000`. | [MarkdownBody](../app/src/components/MarkdownBody.tsx) | [review tests](../app/src/components/MarkdownBody.review.test.tsx): oversized rule no longer creates 10000em styles. |
| 2 | Cache failed suffix bounds by inline parser context and closing delimiter. A failed suffix is scanned once, and the cache cannot leak between messages/renders. | [remarkLatexDelimiters](../app/src/lib/remarkLatexDelimiters.ts) | [review tests](../app/src/components/MarkdownBody.review.test.tsx): 8,000 unclosed openers followed by a streamed update, with a 1,500ms ceiling. Before: 29,494ms; after: 387ms in the recorded isolated run. |
| 3 | Require a committed closing-fence token before typesetting a dollar block. An unclosed node is rendered as readable source. Quote/list containers use parser tokens, not guesses from source text. | [remarkChatMath](../app/src/lib/remarkChatMath.ts) | [review](../app/src/components/MarkdownBody.review.test.tsx) and [streaming](../app/src/components/MarkdownBody.streaming.test.tsx) tests. |
| 4 | Atomic inline formulas get `inline-block`, `max-width: 100%`, and horizontal overflow scrolling. Display formulas retain their existing scroller. | [chatMath.css](../app/src/styles/chatMath.css) | Long formula is available in the product examples. **Real Tauri width, scrolling and clipping measurement not performed here.** |
| 5 | Single-dollar math cannot begin/end with whitespace, end before a digit, or cross a line ending. The parser stops at the first candidate dollar; it does not globally rewrite the source. `$2$` stays valid. | [remarkChatMath](../app/src/lib/remarkChatMath.ts) | [review](../app/src/components/MarkdownBody.review.test.tsx) and [streaming](../app/src/components/MarkdownBody.streaming.test.tsx) tests, including both reported prices and valid math after prices. |
| 6 | Preserve local copy-path metadata in HAST before URL sanitization. `file:///tmp/a%20b.txt` copies `/tmp/a b.txt`; its navigation href stays sanitized. | [rehypeLocalCopyPaths](../app/src/lib/rehypeLocalCopyPaths.ts), [MarkdownFileLink](../app/src/components/MarkdownFileLink.tsx) | [path tests](../app/src/components/MarkdownBody.paths.test.tsx): menu, exact clipboard argument, no read/open, and sanitized href. |
| 7 | Copy paths come from the original Markdown URL decoded exactly once, independently of image preview transformations. Code-span paths remain literal. Lead-summary image rendering receives the same metadata. | [chatFilePath](../app/src/lib/chatFilePath.ts), [localMarkdownImage](../app/src/components/localMarkdownImage.tsx), [LeadSummaryBlock](../app/src/components/LeadSummaryBlock.tsx) | [path tests](../app/src/components/MarkdownBody.paths.test.tsx): `%2520`, `%23`, file URLs, Chinese and spaces, plus literal `%20` in code. |
| 8 | A double-dollar inline AST node is explicitly emitted with display-math rendering metadata. | [remarkChatMath](../app/src/lib/remarkChatMath.ts) | [review](../app/src/components/MarkdownBody.review.test.tsx) and [streaming](../app/src/components/MarkdownBody.streaming.test.tsx) tests cover same-line and separately fenced blocks. |

The render and path suites also retain coverage of code blocks, inline code, links, escapes, ordinary brackets, malformed math, blocked HTML/URL commands and recursive macros. Every character boundary is tested for the formula fixtures. Existing Mermaid/image/link tests remain in the full suite.

## Composer and product examples

The desktop composer has a formula button. It offers inline and display insertion, plus expandable source-and-preview examples. It does not send a message or call a provider.

- Inline: `Cmd/Ctrl+Shift+M`, inserting `\(selection\)`.
- Display: `Cmd/Ctrl+Shift+E`, inserting `\[` and `\]` on separate lines.
- Empty selection: caret lands between delimiters. Selected text stays selected inside the delimiters.
- Uses the existing `insertText` command path. This retains the browser's native `execCommand` undo path; fallback behavior is unchanged.
- Commands leave IME composition alone and respect readonly mode.
- Manual entry of all four supported delimiter forms remains available.
- Product examples include the KL equation in four forms, a long equation, and malformed input. Tests are not the example entry point.

Sources: [ComposerMathTools](../app/src/components/ComposerMathTools.tsx), [mathEdit](../app/src/lib/mathEdit.ts), [keyboard hook](../app/src/hooks/useComposerMarkdownKeys.ts), [examples](../app/src/lib/chatMathExamples.ts). Tests: [composer commands](../app/src/components/ComposerMathTools.test.tsx), existing [insertText tests](../app/src/lib/insertText.test.ts).

Native undo and real IME behavior still require the Tauri check below. Simulated composition and edit-command tests are not that check. Remote-web continues to share chat rendering; the new insertion UI is in the desktop composer.

## Executed checks

| Cwd | Command | Exit/result |
|---|---|---|
| app | `npm test` | 0; 211 files / 2,868 tests; script and filesize checks also pass |
| app | `npm run typecheck` | 0 |
| app | `npm run format:check` | 0 |
| app | `npm run lint` | 0 |
| app | `npm run build` | 0; frontend build, **not** a native Tauri bundle |
| remote-web | `npm run build` | 0 |
| remote-web | `npm test` | 1; 1,156 pass, 4 baseline failures |
| exact Base, remote-web | `./node_modules/.bin/vitest run src/app/AppRuntime.bodyCache.e2e.test.tsx` | 1; 8 pass, the same 4 failures |
| app | `./node_modules/.bin/vitest run src/components/MarkdownBody.review.test.tsx --reporter=verbose` | 0; 9 pass, failed-suffix case 387ms |
| app | `cargo build --release --locked --manifest-path ../harness-agent/Cargo.toml` | 127; cargo not installed |
| app | `rustc -vV` | 127; rustc not installed; sidecar staging cannot proceed |
| app | `cargo test --no-fail-fast --manifest-path src-tauri/Cargo.toml` | 127; cargo not installed |
| harness-agent | `cargo test --no-fail-fast` | 127; cargo not installed |
| harness-agent | `cargo fmt --check` | 127; cargo not installed |

The four remote-web failures have identical test names on the repair and the exact Base: deferred cache lookup, stale revision fallback, repeated stale revision fallback, and rejected fallback fetch. This reproduction used Node 24, whereas the incoming review reported Node 22. These tests were not changed, skipped or weakened. This is not an all-green result.

The frontend build emits 59 KaTeX font assets. Vite emits its existing large-chunk warning; thresholds were not raised. No workflow, Ratchet thresholds or exemptions were added/relaxed. The earlier removal of MarkdownBody's long-function exemption remains effective. New production source files in this repair are at most 130 lines.

## Required local GUI acceptance (not executed here)

There is no Rust toolchain, native Tauri window, or installed browser in this execution environment. Playwright Chromium installation was attempted and failed with a truncated/invalid archive. No screenshot or actual clipboard/undo result is claimed.

Use the real desktop application to finish these checks:

1. Open a session. Open the formula button and expand the formula examples. Confirm fonts with network disabled. Compare the source and preview for each delimiter.
2. Render `$\rule{10000em}{10000em}$`. Check computed dimensions; the supplied dimensions must be capped at 20em, not 10000em.
3. Use the long inline example (`abcdefghij` repeated 40 times inside `\text{...}`). Measure the equation and ancestor widths at a narrow window size and with the right panel open. Confirm horizontal scrolling reaches the last character without widening/clipping the turn. Capture screenshots and widths.
4. Stream partial formulas and the unclosed dollar-block fixture from the review. Normal prose must remain readable before closure; after closure the result must match one-shot rendering.
5. Select text and use both insertion commands. Test empty selections, undo/redo, Chinese IME composition, keyboard navigation, Escape and click-outside dismissal.
6. Right-click the reported file URLs and image URLs. Paste the actual OS clipboard into a plain editor. Confirm exact values (`%2520` → `%20`, `%23` → `#`), and confirm the copy action causes no extra read/open/preview/authorization. Test failure feedback and viewport-edge menu placement.

Cross-directory authorization, the existing image-read decoding behavior, and routing files into the right-side Files panel are outside this repair. Backend file access boundaries were not changed. Keep the PR unmerged pending review and the blocked Rust/native GUI checks.

## Recorded output excerpts

### Before repair: formula fixtures

```text
     × bounds rule dimensions 54ms
     × preserves currency: Price: $5 and $10. 14ms
     × preserves currency: Cost $5
     × displays double dollars: $$x^2$$ 7ms
     × leaves an unclosed block readable (streaming=true) 14ms
     × leaves an unclosed block readable (streaming=false) 13ms
     × keeps numeric math and valid math after prices 10ms
     × bounds repeated failed delimiter scans during streaming 29495ms
⎯⎯⎯⎯⎯⎯⎯ Failed Tests 8 ⎯⎯⎯⎯⎯⎯⎯
AssertionError: expected 29493.741076 to be less than 1500
 Test Files  1 failed | 1 passed (2)
      Tests  8 failed | 10 passed (18)
```

### After repair: full frontend

```text
 Test Files  211 passed (211)
      Tests  2868 passed (2868)
   Duration  43.83s (transform 16.20s, setup 12.21s, import 57.57s, tests 113.97s, environment 78.47s)
Ran 51 tests in 8.228s
OK
```

### After repair: performance

```text
 ✓ src/components/MarkdownBody.review.test.tsx > math review regressions > bounds repeated failed delimiter scans during streaming 387ms
 Test Files  1 passed (1)
      Tests  9 passed (9)
```

### Remote-web

```text
⎯⎯⎯⎯⎯⎯⎯ Failed Tests 4 ⎯⎯⎯⎯⎯⎯⎯
 FAIL  |ui| src/app/AppRuntime.bodyCache.e2e.test.tsx > AppRuntime body cache e2e · ④b 竞跑闸——真挂起 deferred get（msgfix2 U4 修单 H7） > get() 真挂起期间——同一条消息的重复自动拉取尝试被阻断（不发第二次 msg.fetch）；get() 落地（未命中）后正常恢复，只发一次
 FAIL  |ui| src/app/AppRuntime.bodyCache.e2e.test.tsx > AppRuntime body cache e2e · ⑤ stale-revision 缓存拒绝回落（msgfix2 U4 修单 H4） > 缓存查询挂起期间消息已升级到新 revision——命中的旧缓存被投影拒绝后，清掉这条孤儿条目并自动回落到网络拉取新 revision，不永久空转
 FAIL  |ui| src/app/AppRuntime.bodyCache.e2e.test.tsx > AppRuntime body cache e2e · ⑤b 连环 stale-revision 回落上限（msgfix2 U4 修单二 I5） > 连续两轮缓存命中都被投影拒绝（消息在两次查询挂起期间各推进了一次 revision）——只回落重试一次，第二次不符直接跳过缓存走网络，不会无限 get/delete 循环
 FAIL  |ui| src/app/AppRuntime.bodyCache.e2e.test.tsx > AppRuntime body cache e2e · ⑤c 回落分支 startFetch reject 时锁仍会释放（P2） > 回落分支的 startFetch 拒绝——finally 仍释放 cacheLookupPendingRef，之后同一条消息的新 revision 还能再次触发 get()
 Test Files  1 failed | 64 passed (65)
      Tests  4 failed | 1156 passed (1160)
```

### Exact Base remote-web

```text
⎯⎯⎯⎯⎯⎯⎯ Failed Tests 4 ⎯⎯⎯⎯⎯⎯⎯
 Test Files  1 failed (1)
      Tests  4 failed | 8 passed (12)
```

