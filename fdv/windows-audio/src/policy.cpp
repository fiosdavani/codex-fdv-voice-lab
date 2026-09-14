#include "audio_policy.hpp"
#include <algorithm>
#include <cmath>
#include <limits>

namespace vai {
bool ProcessIdentity::operator==(const ProcessIdentity& b) const {
    return pid == b.pid && creation_time_100ns == b.creation_time_100ns && image_path == b.image_path;
}
bool SessionIdentity::operator==(const SessionIdentity& b) const {
    return endpoint_id == b.endpoint_id && instance_id == b.instance_id && process == b.process;
}
bool VoiceClaim::operator==(const VoiceClaim& b) const {
    return thread.value == b.thread.value && native_session.value == b.native_session.value &&
           generation.value == b.generation.value;
}
RenderGate::RenderGate(IRenderPlatform& p, const IRenderAuthority& a) : platform_(p), authority_(a) {}
MuteResult RenderGate::reconcile() {
    last_ = {MuteState::Blocked, "NC_SCOPE_OR_INVENTORY", 0};
    if (!authority_.alive()) return last_;
    const auto snapshot = platform_.snapshot();
    if (!snapshot.complete || snapshot.revision != platform_.revision()) return last_;
    std::vector<SessionIdentity> targets;
    for (const auto& session : snapshot.sessions) {
        const auto role = authority_.classify(session);
        if (role == SessionRole::Unknown) return last_;
        if (role != SessionRole::CodexRender) continue;
        if (!session.render_only || !session.single_process || session.system_sounds ||
            session.identity.instance_id.empty() || session.identity.endpoint_id.empty() ||
            session.identity.process.pid == 0 || session.identity.process.creation_time_100ns == 0 ||
            session.identity.process.image_path.empty()) return last_;
        if (std::find(targets.begin(), targets.end(), session.identity) != targets.end()) return last_;
        targets.push_back(session.identity);
    }
    if (targets.empty()) { last_.reason = "NC_NO_TARGET_RENDER_SESSION"; return last_; }
    // Validate the entire snapshot before writing any session.
    for (const auto& target : targets) {
        if (!authority_.alive() || platform_.revision() != snapshot.revision || !platform_.mute(target)) {
            last_.reason = "NC_MUTE_OR_REVOCATION"; return last_;
        }
    }
    for (const auto& target : targets) {
        const auto muted = platform_.read_mute(target);
        if (!muted || !*muted) { last_.reason = "NC_READBACK"; return last_; }
        ++last_.verified;
    }
    if (!authority_.alive() || platform_.revision() != snapshot.revision) {
        last_.reason = "NC_CHURN"; return last_;
    }
    verified_revision_ = snapshot.revision;
    if (!platform_.prospective_render_barrier_held()) {
        last_.state = MuteState::ExistingSessionsVerified;
        last_.reason = "NC_FUTURE_SESSION_PRE_RENDER_BARRIER";
        return last_;
    }
    last_.state = MuteState::Ready;
    last_.reason = "READY"; // reachable in mocks only with current adapter.
    return last_;
}
bool RenderGate::ready() const {
    return last_.state == MuteState::Ready && authority_.alive() &&
           platform_.revision() == verified_revision_ && platform_.prospective_render_barrier_held();
}
OnsetDetector::OnsetDetector(const IVoiceScopeVerifier& s, const IAcousticAssurance& a, IOnsetSink& out)
    : scopes_(s), acoustics_(a), sink_(out) {}
void OnsetDetector::reset_activity() {
    first_hot_ = hot_duration_ = quiet_duration_ = 0;
    hot_acoustics_verified_ = true;
    talking_ = false;
}
OnsetStatus OnsetDetector::push(const CapturePacket& p, const VoiceClaim& claim, const ScopeSeal& seal) {
    if (invalidated_) return OnsetStatus::Rejected;
    if (claim.thread.value.empty() || claim.native_session.value.empty() || claim.generation.value == 0 ||
        !scopes_.verify(claim, seal, p.qpc_100ns)) {
        reset_activity(); return OnsetStatus::Rejected;
    }
    // One detector belongs to one sealed generation. A new generation needs a
    // fresh detector; delayed old-generation packets cannot retarget it.
    if (scope_ && !(*scope_ == claim)) { invalidated_ = true; return OnsetStatus::Rejected; }
    if (!scope_) { scope_ = claim; endpoint_ = p.endpoint_id; endpoint_revision_ = p.endpoint_revision; }
    if (p.endpoint_id.empty() || p.endpoint_id != endpoint_ || p.endpoint_revision != endpoint_revision_ ||
        p.discontinuity || p.timestamp_error || p.sample_rate < 8000 || p.sample_rate > 192000 ||
        p.channels == 0 || p.channels > 8 || p.samples.empty() ||
        p.samples.size() % p.channels != 0 || p.qpc_100ns == 0 ||
        p.qpc_100ns <= last_packet_ || (next_packet_min_ > p.qpc_100ns && next_packet_min_ - p.qpc_100ns > 1000)) {
        invalidated_ = true; return OnsetStatus::Rejected;
    }
    const auto frames = p.samples.size() / p.channels;
    const auto duration = static_cast<std::uint64_t>(frames) * 10000000ULL / p.sample_rate;
    if (!duration || duration > 1000000 || p.qpc_100ns > std::numeric_limits<std::uint64_t>::max() - duration) {
        invalidated_ = true; return OnsetStatus::Rejected;
    }
    if (next_packet_min_ && p.qpc_100ns > next_packet_min_ + 200000) {
        invalidated_ = true; return OnsetStatus::Rejected;
    }
    last_packet_ = p.qpc_100ns;
    next_packet_min_ = p.qpc_100ns + duration;
    double sum = 0;
    for (const auto x : p.samples) {
        if (!std::isfinite(x) || std::abs(x) > 1.0F) { invalidated_ = true; return OnsetStatus::Rejected; }
        sum += static_cast<double>(x) * x;
    }
    const double rms = std::sqrt(sum / static_cast<double>(p.samples.size()));
    if (rms >= 0.025) { // engineering fixture threshold; NOT measured speech VAD.
        quiet_duration_ = 0;
        if (hot_duration_ == 0) { first_hot_ = p.qpc_100ns; hot_acoustics_verified_ = true; }
        hot_acoustics_verified_ = hot_acoustics_verified_ && acoustics_.near_end_speech_verified(p);
        hot_duration_ = std::min<std::uint64_t>(hot_duration_ + duration, 10000000);
        if (talking_ || hot_duration_ < 200000) return OnsetStatus::Quiet;
        talking_ = true;
        if (!hot_acoustics_verified_) return OnsetStatus::CandidateUnverified;
        // Re-check first sample scope too: an onset must not precede generation.
        if (!scopes_.verify(claim, seal, first_hot_) || sequence_ == std::numeric_limits<std::uint64_t>::max())
            return OnsetStatus::Rejected;
        sink_.local_onset({claim, ++sequence_, first_hot_, p.endpoint_id});
        return OnsetStatus::Emitted;
    }
    hot_duration_ = 0;
    quiet_duration_ = std::min<std::uint64_t>(quiet_duration_ + duration, 10000000);
    if (quiet_duration_ >= 1500000) talking_ = false;
    return OnsetStatus::Quiet;
}
} // namespace vai
