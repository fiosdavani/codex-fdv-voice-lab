#pragma once
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace vai {
struct ProcessIdentity {
    std::uint32_t pid{};
    std::uint64_t creation_time_100ns{};
    std::wstring image_path;
    bool operator==(const ProcessIdentity& b) const;
};
struct SessionIdentity {
    std::wstring endpoint_id;
    std::wstring instance_id;
    ProcessIdentity process;
    bool operator==(const SessionIdentity& b) const;
};
struct AudioSession {
    SessionIdentity identity;
    bool render_only{};
    bool single_process{};
    bool system_sounds{};
};
struct RenderSnapshot {
    std::uint64_t revision{};
    // false if process tree/session inventory has gaps, denied access, or churn.
    bool complete{};
    std::vector<AudioSession> sessions;
};
class IRenderPlatform {
public:
    virtual ~IRenderPlatform() = default;
    virtual RenderSnapshot snapshot() = 0;
    virtual bool mute(const SessionIdentity&) = 0; // TRUE only; no endpoint/mic API.
    virtual std::optional<bool> read_mute(const SessionIdentity&) = 0;
    virtual std::uint64_t revision() const = 0;
    // Must intercept every future render BEFORE first sample. Notifications are
    // insufficient. Windows adapter always returns false; true exists in mocks.
    virtual bool prospective_render_barrier_held() const = 0;
};
enum class SessionRole { CodexRender, OwnPlayer, Unrelated, Unknown };
class IRenderAuthority {
public:
    virtual ~IRenderAuthority() = default;
    // Trusted host must bind exact session instance + process creation + image.
    // A PID/process name/descendant relation is discovery, never authorization.
    virtual SessionRole classify(const AudioSession&) const = 0;
    virtual bool alive() const = 0;
};
enum class MuteState { Blocked, ExistingSessionsVerified, Ready };
struct MuteResult {
    MuteState state{MuteState::Blocked};
    std::string reason;
    std::size_t verified{};
};
class RenderGate {
    IRenderPlatform& platform_;
    const IRenderAuthority& authority_;
    MuteResult last_;
    std::uint64_t verified_revision_{};
public:
    RenderGate(IRenderPlatform& platform, const IRenderAuthority& authority);
    MuteResult reconcile();
    bool ready() const;
};

struct ThreadId { std::string value; };
struct NativeSessionId { std::string value; };
struct VoiceGeneration { std::uint64_t value{}; };
struct VoiceClaim {
    ThreadId thread;
    NativeSessionId native_session;
    VoiceGeneration generation;
    bool operator==(const VoiceClaim& b) const;
};
struct ScopeSeal { std::vector<std::uint8_t> opaque; };
class IVoiceScopeVerifier {
public:
    virtual ~IVoiceScopeVerifier() = default;
    // External authenticated native voice lifecycle; must check seal, claim,
    // generation, expiry, revocation and timestamp >= voice generation start.
    virtual bool verify(const VoiceClaim&, const ScopeSeal&,
                        std::uint64_t packet_qpc_100ns) const = 0;
};
struct CapturePacket {
    std::wstring endpoint_id;
    std::uint64_t endpoint_revision{};
    std::uint64_t qpc_100ns{}; // WASAPI packet start; monotonic, NOT UTC.
    std::uint32_t sample_rate{};
    std::uint16_t channels{};
    std::vector<float> samples; // interleaved; only in RAM; never logged.
    bool discontinuity{};
    bool timestamp_error{};
};
class IMicrophone {
public:
    virtual ~IMicrophone() = default;
    virtual std::optional<CapturePacket> next_packet() = 0;
};
class IAcousticAssurance {
public:
    virtual ~IAcousticAssurance() = default;
    // Must cover actual endpoint/revision + current own-player state and prove
    // near-end speech attribution. Energy threshold alone cannot supply this.
    virtual bool near_end_speech_verified(const CapturePacket&) const = 0;
};
struct OnsetEvent {
    VoiceClaim scope;
    std::uint64_t onset_sequence{};
    std::uint64_t onset_qpc_100ns{};
    std::wstring endpoint_id;
};
class IOnsetSink {
public:
    virtual ~IOnsetSink() = default;
    // Local own-player stop hint only. There is NO backend cancel interface.
    // Receiving host must independently validate current scope and sequence.
    virtual void local_onset(const OnsetEvent&) = 0;
};
enum class OnsetStatus { Quiet, CandidateUnverified, Emitted, Rejected };
class OnsetDetector {
    const IVoiceScopeVerifier& scopes_;
    const IAcousticAssurance& acoustics_;
    IOnsetSink& sink_;
    std::optional<VoiceClaim> scope_;
    std::wstring endpoint_;
    std::uint64_t endpoint_revision_{};
    std::uint64_t last_packet_{};
    std::uint64_t next_packet_min_{};
    std::uint64_t first_hot_{};
    std::uint64_t hot_duration_{};
    std::uint64_t quiet_duration_{};
    std::uint64_t sequence_{};
    bool hot_acoustics_verified_{true};
    bool talking_{};
    bool invalidated_{};
    void reset_activity();
public:
    OnsetDetector(const IVoiceScopeVerifier&, const IAcousticAssurance&, IOnsetSink&);
    OnsetStatus push(const CapturePacket&, const VoiceClaim&, const ScopeSeal&);
};
// Fail-closed production defaults. No claim can make these grant authority.
class UnavailableScopeVerifier final : public IVoiceScopeVerifier {
    bool verify(const VoiceClaim&, const ScopeSeal&, std::uint64_t) const override { return false; }
};
class UnverifiedAcoustics final : public IAcousticAssurance {
    bool near_end_speech_verified(const CapturePacket&) const override { return false; }
};
} // namespace vai
