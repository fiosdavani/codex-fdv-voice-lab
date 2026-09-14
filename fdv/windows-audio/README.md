# Windows audio helper candidate — 2026-09-14

**Native integration is NOT READY. A and B remain NC.** This directory contains actual Windows Core Audio/WASAPI adapter source and portable policy tests. It contains no authorized host integration, no production scope verifier, and no measured hardware result. The only executable entry point is `--capabilities`, which prints static limitations without initializing COM, enumerating devices, changing volume, or capturing audio.

## What exists

`src/windows_adapter.cpp` implements a render-session adapter and a shared microphone adapter, behind the interfaces in `include/audio_policy.hpp`. Neither is called by the executable or tests. The native library uses C++17, Microsoft WRL from the Windows SDK, `ole32` and `uuid`; no package installation, driver, administrator privilege, external dependency, PowerShell policy change, or service is required by the source. Normal-user access can still fail; permission failure is NC, not permission to elevate.

The render adapter enumerates every active **render** endpoint and binds each enumerated `IAudioSessionControl2` to `ISimpleAudioVolume`. It can call only `SetMute(TRUE)` and `GetMute` for these bound session objects. It does not expose endpoint volume/mute, capture-session mute, unmute, own-player output control, or backend-turn cancellation. Existing-session verification requires successful SetMute followed by successful GetMute=true for **every authorized Codex render session**. Zero targets, false/error readback, incomplete inventory, ambiguous multi-process sessions, missing identity, unknown grants, and detected churn block readiness.

Session roles come from an external `IRenderAuthority`: exact endpoint ID + audio-session instance ID + PID + process creation time + image path. Codex rendering and VAI own-player sessions must be positively distinguished; OwnPlayer and Unrelated roles are excluded. The root PID is supplied as an expected process identity. Toolhelp process snapshots discover descendants, including grandchildren, and process creation times reduce PID-reuse mistakes. Discovery never grants authorization. A broker outside that tree, same-process mixed sessions, inaccessible process identity, old surviving sessions, and escaped/reparented renderers need additional attribution evidence; these APIs alone cannot prove complete Codex ownership. The host must supply immutable grants for one lifecycle, with revocation exposed through `alive()`.

Endpoint and session notifications increment an atomic revision. Registration occurs before `GetSessionEnumerator`/`GetCount` on an MTA worker. New session creation, endpoint/default changes, disconnection, external unmute, and changed process-tree membership invalidate the observation epoch. Callbacks do no blocking work and do not mute or release audio objects. The adapter never treats a later enumeration as automatically complete: any detected churn requires disposing the adapter, rediscovering, and obtaining new external grants.

## Why A is still NC

Microsoft's notifications report session creation; they are not a hook that prevents the first sample from rendering. SetMute/GetMute only verifies a session that already exists. The adapter's `prospective_render_barrier_held()` therefore **always returns false**, so its strongest possible result is `ExistingSessionsVerified`, never `Ready`. A new process/session can render before an observer reacts. Polling, callbacks, persistent session settings, and a successful readback do not close that gap.

The `Ready` branch is exercised only by a fake platform with a synthetic pre-render barrier. It proves policy ordering under that assumption, not Windows behavior. Readiness is revocable and must never be cached by a caller. Existing-session mutation, if later authorized, is not evidence that there was no prior sound or no future sound.

Accepting A requires a proven native barrier that covers every current/future Codex render path **before the first frame** and survives process/session churn, restart, endpoint migration, external unmute, disconnect and service errors. It must also prove that VAI playback and the microphone are unaffected. No such native barrier or live grant provider is implemented here. No state in this helper licenses activation of the voice path while this remains NC.

## Why B is still NC

The microphone factory requires an explicit capture endpoint ID and uses WASAPI shared capture, the mix format, a 100 ms buffer request, and timer-driven packet reads. It supports normalized float32 and PCM16; unsupported formats fail with NC. The caller must drain available packets promptly from the same worker that owns the COM objects. Samples are held in memory only. Silence flags are honored; discontinuities, timestamp errors, invalid samples and endpoint changes fail closed. The adapter does not seize exclusive access or change microphone mute.

Shared mode permits coexistence at the API level when the device is available for shared use. Successful coexistence with the actual Codex microphone stream, device privacy settings, hardware/driver behavior and latency has **not** been measured. An exclusive stream, denied access, device loss, unsupported format or capture error is NC. The source targets Windows 10/11; its COM worker does not attempt compatibility with Windows 8's first-use STA caveat.

The local detector is a deliberately small **energy-onset candidate**, using RMS >= 0.025 for 20 ms, with 150 ms quiet rearming. These thresholds are engineering fixtures, not calibrated speech detection. Own-player acoustic echo, loudspeaker output, music, keyboard noise and room sounds can all cross them. Before emitting an actionable local hint, every hot packet needs external `IAcousticAssurance` for near-end speech on the actual endpoint/revision and current own-player state. Its production default is `UnverifiedAcoustics`, which never verifies. No echo cancellation or speaker/user identity inference is claimed.

Onset carries typed thread ID, native session ID, generation, monotonic onset sequence and the first hot packet's QPC timestamp. WASAPI QPC timestamps are already in 100 ns units and are **not UTC**. The timestamp does not identify a speaker or authorize a task. `IVoiceScopeVerifier` must validate an external opaque seal, current native voice lifecycle, exact claim, generation start, expiry, live revocation, current time and packet age. Its production default rejects everything. A caller-supplied PID, text/internal event, thread string or JSON boolean cannot substitute for that verifier. A detector is tied to one generation and endpoint epoch; a changed generation or invalidated endpoint needs a fresh externally authorized detector. The host must prevent two detectors from issuing duplicate sequence numbers for the same generation.

The only sink method is `local_onset(OnsetEvent)`, intended solely for a separately authorized local own-player stop. The receiver must independently revalidate live scope and deduplicate sequence numbers. There is no backend cancellation method, no text/internal turn hook, no transcript transport, no queue/routing integration, and no Rust change in this directory. The actual VAI player bridge is absent. B needs authenticated lifecycle binding, real speech/echo validation, coexistence and latency measurements before it can be accepted.

## Authorized CI build and mocks

Use a Windows GitHub Actions runner with the existing Visual Studio C++ workload, Windows 10/11 SDK and CMake >= 3.20. From the parent containing this folder:

```text
cmake -S windows-audio -B build-windows-audio -A x64
cmake --build build-windows-audio --config Release
ctest --test-dir build-windows-audio -C Release --output-on-failure -V
```

All C++ targets compile with `/W4 /WX /permissive-` on MSVC. CTest runs two executables: `audio_policy_tests` (42 synthetic named test cases) and `windows_audio_candidate --capabilities` (static text only). This is not 42 CTest executables. `policy-mocks-receipt.json` in the build directory records every case name/status and the executed assertion count. Preserve this receipt and verbose CTest/build logs as CI artifacts. Publish only source files from this directory, not the Linux test executable or local build outputs.

For an already-provisioned Linux compiler, portable policy tests require only:

```text
g++ -std=c++17 -Wall -Wextra -Werror -pedantic -I include src/policy.cpp tests/policy_tests.cpp -o audio_policy_tests
./audio_policy_tests --receipt policy-mocks-receipt.json
```

These commands exercise no Windows/audio APIs. A successful native Windows build proves compilation and linking only. An eventual live test on Alessandro's PC still requires a separate explicit authorization; no PC execution was performed or scheduled in this subtask.

Primary Microsoft documentation and source-to-claim references are in `SOURCES.md`. Local test evidence is in `evidence/`; the root task records the Windows CI outcome separately.
