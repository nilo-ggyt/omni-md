# Optional native source-input evidence

`omni-md-plugin/native-input-trace` is a separate, nondefault instrumentation feature. It records
finite scalar evidence for the actual product editor. The default binary contains no trace ivars,
trace state, keyDown override, recording branch, observer or timer. Enabling this feature changes
instrumented input cost; its records do not by themselves establish performance acceptance.

The feature owns one shared recorder inside the current editor's view/storage graph. That recorder
contains no document, source string, event, native-object owner, filesystem path, callback or global
history. It admits at most 256 records, eight nested event scopes and eight nested selector scopes
per stack. Capacity exhaustion preserves the first records and invalidates the sample. Dropping the editor discards undrained scalar evidence;
there is no output, timer, observer, Drop flush or periodic flush.

## Explicit measurement boundary

Call `arm_native_markdown_input_trace(MainThreadMarker)` only after preparing the isolated synthetic
fixture, selecting actual Source presentation, arranging its visible ROI and choosing the actual
input source. Arming rejects existing composition, an already armed run, absent editors or busy
foreground ownership. The initial O(1) document counters record the loaded UTF-8/UTF-16 extent and
generation. For a 1 MiB ASCII test, the caller must independently construct and validate the exact
1,048,576-byte ASCII fixture and declare its line shape; counters alone cannot prove ASCII content.

Keep the same document, Source presentation, editor focus and declared ROI throughout the armed
interval. Arming checks presentation only at entry; `is_valid()` checks recorder integrity and cannot
attest that the caller preserved those measurement conditions. A focus, presentation or document
change invalidates the external measurement procedure even if its scalar snapshot remains valid.

After actual keyboard/IME interaction and external frame collection have ended, call
`drain_native_markdown_input_trace(MainThreadMarker)` before closing or changing the document.
The returned `NativeInputTraceSnapshot` owns only fixed scalar records. The trace-state borrow and
foreground-owner lease end before the caller receives it, so the fixture may format/write those
records afterwards. Draining during a native input scope rejects rather than splitting its evidence.
The caller must inspect `is_valid()` and use `records()`; Debug output is not a measurement protocol.

There is intentionally no automatic logging or public API for injecting input. Physical/manual or
OS-delivered input provenance belongs to the actual test procedure. Receiving an NSEvent does not
by itself certify that an event originated from a physical HID device. Directly calling input-client
selectors, typing a whole Chinese string through a paste path, or using the synthetic runtime probe
does not establish real system Chinese IME behavior.

## Causal records and canonical outcomes

A feature-only keyDown override records the exact `NSEvent.timestamp` f64 bits and enters a scoped
event serial. It forwards the original event to its superclass exactly once. Standard native input
manager handling remains unchanged. `insertText`, `setMarkedText` and `unmarkText` enter their own
nested selector scopes. Only callbacks executing inside that actual keyDown stack receive its event
serial. An asynchronous IME callback has `event_serial=None`; the last application event is never
used to guess a cause. A composition serial can span multiple events without retaining its text.

The storage primitive records the actual canonical Result before native notifications: accepted or
one finite rejection class, generation, current document lengths and removed/inserted UTF-16 counts.
Counts are zero when admission rejects before inspecting the replacement or command preparation
returns no canonical receipt. Direct formatting/history publishers are also visible but have no
invented text-input selector. No source offsets, characters,
input strings, paths, arbitrary error messages, window titles or screenshots enter the trace.

| Outcome | Meaning |
| --- | --- |
| `Provisional` | An outer `setMarkedText` returned. Canonical generation changes are still provisional. |
| `Nested` | A nested selector returned; nested unmark cannot prematurely finalize its outer insertion. |
| `CommittedChanged` | An outer insertion/unmark accepted canonical mutation and actual superclass marked state is clear. |
| `CommittedExisting` | Existing marked text was accepted without a new canonical generation or primitive invocation. |
| `Rejected` | At least one admission/canonical rejection vetoed the outer operation's commit claim. |
| `NoCommit` | A no-op, remaining native marking or missing accepted mutation establishes no completed insertion. |

Canonical-decision records leave `native_marked=None`; only entry/exit observations assert an actual
superclass marked-state value. Scope unwinding restores scalar provenance and invalidates the sample
without recording a fabricated completion or doing output. A different key event reentering an
unfinished selector also invalidates causal evidence.

## Clocks and external semantic observation

All callback and canonical-decision boundaries use raw `mach_absolute_time` ticks. The snapshot
includes the exact `mach_timebase_info` numerator/denominator. Convert differences with checked wide
integer arithmetic. ScreenCaptureKit display Mach ticks can be correlated in that same domain.
Raw `NSEvent.timestamp` is retained separately: do not assume its seconds have the identical epoch
as raw Mach ticks or subtract those raw values. This implementation introduces no undocumented
clock bridge and does not use wall time as an event latency clock.

An admissible result is the time from actual keyDown entry, or an explicitly unassociated commit
completion, to the **first observed complete semantic NEW frame** in the verified source ROI. A draw
callback, native edit result, application update, generic Complete frame or adjacent unchanged ASCII
marker cannot prove that the intended source content was displayed. Capture sampling can miss an
earlier frame, so the first observed matching frame is an observation upper bound, not a physical
screen-photon timestamp. The event-occurrence-to-display metric remains unavailable until a justified
clock bridge and its uncertainty are established.

The external observer must first positively recognize OLD and NEW absence before input. A Chinese
run also needs a separately validated Chinese-language recognition/oracle configuration; the current
English-only nonce recognizer does not establish Chinese glyph recognition. Collect bounded
continuous OLD/NEW predicates rather than stopping at a provisional NEW candidate. If NEW appeared
before native commit completion, classify the sample `precommit_visible` and omit committed-paint
latency unless a separately validated marked-versus-unmarked pixel oracle exists. Identical Chinese
glyphs can already be visible provisionally. No diagnostic marker can replace that semantic proof.

Primary contracts: [NSEvent timestamp](https://developer.apple.com/documentation/appkit/nsevent/timestamp),
[standard input interpretation](https://developer.apple.com/documentation/appkit/nsresponder/interpretkeyevents(_:)),
[user insertText](https://developer.apple.com/documentation/appkit/nstextinputclient/inserttext(_:replacementrange:)),
[unmark accepts existing text](https://developer.apple.com/documentation/appkit/nstextinputclient/unmarktext()),
[Mach absolute clock](https://developer.apple.com/documentation/kernel/1462446-mach_absolute_time), and
[WindowServer frame display time](https://developer.apple.com/documentation/screencapturekit/scstreamframeinfo/displaytime).

## Executed source checks and remaining acceptance

The pure trace tests exercise synchronous provenance, asynchronous IME association absence,
provisional generation changes, nested unmark, existing-text commit, canonical rejection, record and
scope capacity, explicit control reentry, and unwind restoration. They allocate no native application.
The feature is also compiled with the normal native sources and warning-denied Clippy/rustdoc gates.
No real keyboard, system Chinese IME, capture permission, source ROI or completed-paint performance
run is claimed by those tests. macOS 14 remains the deployment floor; actual macOS 14 and M1/8 GiB
validation remain unmeasured. This feature closes no baseline retirement or performance gate itself.
