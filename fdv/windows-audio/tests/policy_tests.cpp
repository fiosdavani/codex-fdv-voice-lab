#include "audio_policy.hpp"
#include <algorithm>
#include <functional>
#include <fstream>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <utility>

using namespace vai;
namespace {
std::size_t assertions_executed = 0;
void require(bool condition, const char* detail) {
    ++assertions_executed;
    if (!condition) throw std::runtime_error(detail);
}
#define CHECK(condition) require((condition), #condition)
AudioSession session(std::wstring id, std::uint32_t pid) {
    return {{L"render-A", std::move(id), {pid, 123400 + pid, L"C:\\fixture\\app.exe"}}, true, true, false};
}
struct FakeRender final : IRenderPlatform {
    std::uint64_t rev{1};
    bool complete{true};
    bool barrier{true}; // synthetic pre-render hook; NOT a Windows capability.
    bool fail_mute{};
    bool fail_read{};
    bool false_read{};
    bool churn_on_mute{};
    bool churn_on_read{};
    std::vector<AudioSession> inventory{session(L"codex-root", 100), session(L"codex-child", 101),
                                        session(L"own-player", 200), session(L"music", 300)};
    std::vector<SessionIdentity> writes;
    std::vector<SessionIdentity> reads;
    RenderSnapshot snapshot() override { return {rev, complete, inventory}; }
    bool mute(const SessionIdentity& s) override {
        writes.push_back(s);
        if (churn_on_mute) ++rev;
        return !fail_mute;
    }
    std::optional<bool> read_mute(const SessionIdentity& s) override {
        reads.push_back(s);
        if (churn_on_read) ++rev;
        if (fail_read) return std::nullopt;
        return !false_read;
    }
    std::uint64_t revision() const override { return rev; }
    bool prospective_render_barrier_held() const override { return barrier; }
};
struct FakeAuthority final : IRenderAuthority {
    bool active{true};
    std::vector<AudioSession> grants;
    explicit FakeAuthority(const FakeRender& p) : grants(p.inventory) {}
    bool alive() const override { return active; }
    SessionRole classify(const AudioSession& s) const override {
        for (const auto& grant : grants) {
            if (!(s.identity == grant.identity)) continue;
            if (s.identity.instance_id == L"own-player") return SessionRole::OwnPlayer;
            if (s.identity.instance_id == L"music") return SessionRole::Unrelated;
            return SessionRole::CodexRender;
        }
        return SessionRole::Unknown;
    }
};
const VoiceClaim voice{{"thread-fixture"}, {"native-fixture"}, {7}};
const ScopeSeal seal{{11, 22, 33}};
struct FakeScopes final : IVoiceScopeVerifier {
    bool active{true};
    std::uint64_t start{10000000};
    std::uint64_t expires{20000000};
    bool verify(const VoiceClaim& c, const ScopeSeal& s, std::uint64_t t) const override {
        return active && c == voice && s.opaque == seal.opaque && t >= start && t < expires;
    }
};
struct FakeAcoustics final : IAcousticAssurance {
    bool verified{true}; // synthetic oracle; no hardware measurement.
    bool near_end_speech_verified(const CapturePacket&) const override { return verified; }
};
struct FakeSink final : IOnsetSink {
    std::vector<OnsetEvent> events;
    void local_onset(const OnsetEvent& e) override { events.push_back(e); }
};
CapturePacket packet(std::uint64_t qpc = 10000000, float amplitude = 0.1F) {
    return {L"mic-A", 1, qpc, 48000, 1, std::vector<float>(480, amplitude), false, false};
}
void two_hot(OnsetDetector& vad) {
    CHECK(vad.push(packet(), voice, seal) == OnsetStatus::Quiet);
    CHECK(vad.push(packet(10100000), voice, seal) == OnsetStatus::Emitted);
}
} // namespace

int main(int argc, char** argv) {
    if (!(argc == 1 || (argc == 3 && std::string(argv[1]) == "--receipt"))) return 2;
    std::size_t passed = 0, failed = 0;
    std::vector<std::pair<std::string, bool>> cases;
    const auto test = [&](const char* name, const std::function<void()>& run) {
        try { run(); ++passed; cases.emplace_back(name, true); std::cout << "PASS " << name << '\n'; }
        catch (const std::exception& e) {
            ++failed; cases.emplace_back(name, false); std::cerr << "FAIL " << name << ": " << e.what() << '\n';
        }
    };
    test("set_true_and_readback_every_target_before_fake_ready", [] {
        FakeRender p; FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(!gate.ready()); CHECK(gate.reconcile().state == MuteState::Ready);
        CHECK(gate.ready()); CHECK(p.writes.size() == 2); CHECK(p.reads == p.writes);
    });
    test("ownplayer_unrelated_excluded", [] {
        FakeRender p; FakeAuthority a(p); RenderGate gate(p, a); gate.reconcile();
        for (const auto& write : p.writes) CHECK(write.process.pid == 100 || write.process.pid == 101);
    });
    test("readback_false_never_ready", [] {
        FakeRender p; p.false_read = true; FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(!gate.ready());
    });
    test("readback_error_never_ready", [] {
        FakeRender p; p.fail_read = true; FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(!gate.ready());
    });
    test("setmute_failure_never_ready", [] {
        FakeRender p; p.fail_mute = true; FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.reads.empty());
    });
    test("future_session_barrier_missing_is_nc", [] {
        FakeRender p; p.barrier = false; FakeAuthority a(p); RenderGate gate(p, a);
        const auto out = gate.reconcile();
        CHECK(out.state == MuteState::ExistingSessionsVerified); CHECK(out.verified == 2);
        CHECK(!gate.ready()); CHECK(out.reason == "NC_FUTURE_SESSION_PRE_RENDER_BARRIER");
    });
    test("no_codex_session_is_not_vacuous_ready", [] {
        FakeRender p; p.inventory.clear(); FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("unknown_new_child_blocks_before_any_write", [] {
        FakeRender p; FakeAuthority a(p); p.inventory.push_back(session(L"new-child", 102));
        RenderGate gate(p, a); CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("pid_reuse_without_exact_birth_grant_rejected", [] {
        FakeRender p; FakeAuthority a(p); ++p.inventory[0].identity.process.creation_time_100ns;
        RenderGate gate(p, a); CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("image_path_or_session_instance_is_not_pid_authority", [] {
        FakeRender p; FakeAuthority a(p); p.inventory[0].identity.process.image_path = L"C:\\other.exe";
        RenderGate gate(p, a); CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("multi_process_session_rejected", [] {
        FakeRender p; FakeAuthority a(p); p.inventory[0].single_process = false;
        RenderGate gate(p, a); CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("capture_session_cannot_be_muted", [] {
        FakeRender p; FakeAuthority a(p); p.inventory[0].render_only = false;
        RenderGate gate(p, a); CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("system_session_cannot_be_muted", [] {
        FakeRender p; FakeAuthority a(p); p.inventory[0].system_sounds = true;
        RenderGate gate(p, a); CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("incomplete_inventory_blocks_before_write", [] {
        FakeRender p; p.complete = false; FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("duplicate_session_inventory_rejected", [] {
        FakeRender p; p.inventory.push_back(p.inventory[0]); FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("churn_during_muting_revokes", [] {
        FakeRender p; p.churn_on_mute = true; FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(!gate.ready());
    });
    test("churn_during_readback_revokes", [] {
        FakeRender p; p.churn_on_read = true; FakeAuthority a(p); RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(!gate.ready());
    });
    test("future_session_notification_revokes_previous_ready", [] {
        FakeRender p; FakeAuthority a(p); RenderGate gate(p, a); gate.reconcile(); CHECK(gate.ready());
        ++p.rev; CHECK(!gate.ready());
    });
    test("revoked_scope_or_lost_barrier_revokes_previous_ready", [] {
        FakeRender p; FakeAuthority a(p); RenderGate gate(p, a); gate.reconcile();
        a.active = false; CHECK(!gate.ready()); a.active = true; p.barrier = false; CHECK(!gate.ready());
    });
    test("expired_grant_produces_zero_mutation", [] {
        FakeRender p; FakeAuthority a(p); a.active = false; RenderGate gate(p, a);
        CHECK(gate.reconcile().state == MuteState::Blocked); CHECK(p.writes.empty());
    });
    test("synthetic_verified_onset_has_scope_sequence_first_packet_qpc", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); two_hot(vad);
        CHECK(out.events.size() == 1); const auto& e = out.events.front();
        CHECK(e.scope == voice); CHECK(e.onset_sequence == 1); CHECK(e.onset_qpc_100ns == 10000000);
        CHECK(e.endpoint_id == L"mic-A");
    });
    test("silence_emits_nothing", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        CHECK(vad.push(packet(10000000, 0), voice, seal) == OnsetStatus::Quiet); CHECK(out.events.empty());
    });
    test("continuous_hot_not_repeated", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); two_hot(vad);
        CHECK(vad.push(packet(10200000), voice, seal) == OnsetStatus::Quiet); CHECK(out.events.size() == 1);
    });
    test("release_then_second_onset_monotonic_sequence", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); two_hot(vad);
        for (std::uint64_t i = 2; i < 17; ++i) vad.push(packet(10000000 + i * 100000, 0), voice, seal);
        vad.push(packet(11700000), voice, seal); vad.push(packet(11800000), voice, seal);
        CHECK(out.events.size() == 2); CHECK(out.events[1].onset_sequence == 2);
    });
    test("rms_without_acoustic_assurance_is_candidate_only", [] {
        FakeScopes s; UnverifiedAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        vad.push(packet(), voice, seal);
        CHECK(vad.push(packet(10100000), voice, seal) == OnsetStatus::CandidateUnverified); CHECK(out.events.empty());
    });
    test("ownplayer_echo_in_first_packet_cannot_become_verified_onset", [] {
        FakeScopes s; FakeAcoustics a; a.verified = false; FakeSink out; OnsetDetector vad(s, a, out);
        vad.push(packet(), voice, seal); a.verified = true;
        CHECK(vad.push(packet(10100000), voice, seal) == OnsetStatus::CandidateUnverified); CHECK(out.events.empty());
    });
    test("missing_native_scope_provider_fails_closed", [] {
        UnavailableScopeVerifier s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        CHECK(vad.push(packet(), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("forged_text_or_internal_scope_has_no_seal", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        CHECK(vad.push(packet(), voice, {{}}) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("different_thread_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        auto bad = voice; bad.thread.value = "text-thread";
        CHECK(vad.push(packet(), bad, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("different_native_session_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        auto bad = voice; bad.native_session.value = "internal-or-stale";
        CHECK(vad.push(packet(), bad, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("stale_generation_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        auto bad = voice; --bad.generation.value;
        CHECK(vad.push(packet(), bad, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("revoked_scope_cannot_emit", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        vad.push(packet(), voice, seal); s.active = false;
        CHECK(vad.push(packet(10100000), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("packet_before_generation_start_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        CHECK(vad.push(packet(9999999), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("expired_scope_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        CHECK(vad.push(packet(s.expires), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("endpoint_change_invalidates_detector", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        vad.push(packet(), voice, seal); auto changed = packet(10100000); changed.endpoint_id = L"mic-B";
        CHECK(vad.push(changed, voice, seal) == OnsetStatus::Rejected);
        CHECK(vad.push(packet(10200000), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("endpoint_revision_change_invalidates_detector", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out);
        vad.push(packet(), voice, seal); auto changed = packet(10100000); ++changed.endpoint_revision;
        CHECK(vad.push(changed, voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("data_discontinuity_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); auto p = packet();
        p.discontinuity = true; CHECK(vad.push(p, voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("timestamp_error_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); auto p = packet();
        p.timestamp_error = true; CHECK(vad.push(p, voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("replay_packet_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); vad.push(packet(), voice, seal);
        CHECK(vad.push(packet(), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("overlap_or_large_gap_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); vad.push(packet(), voice, seal);
        CHECK(vad.push(packet(10000001), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
        OnsetDetector second(s, a, out); second.push(packet(), voice, seal);
        CHECK(second.push(packet(10400000), voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("nan_or_clipping_outside_normalized_range_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); auto p = packet();
        p.samples[0] = std::numeric_limits<float>::quiet_NaN();
        CHECK(vad.push(p, voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
        OnsetDetector second(s, a, out); p = packet(); p.samples[0] = 1.5F;
        CHECK(second.push(p, voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    test("bad_format_or_empty_packet_rejected", [] {
        FakeScopes s; FakeAcoustics a; FakeSink out; OnsetDetector vad(s, a, out); auto p = packet();
        p.samples.clear(); CHECK(vad.push(p, voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
        OnsetDetector second(s, a, out); p = packet(); p.channels = 0;
        CHECK(second.push(p, voice, seal) == OnsetStatus::Rejected); CHECK(out.events.empty());
    });
    const auto receipt = [&](std::ostream& stream) {
        stream << "{\"kind\":\"OFFLINE_SYNTHETIC_POLICY\",\"count_unit\":\"test_cases\",\"passed\":" << passed
               << ",\"failed\":" << failed << ",\"assertions_executed\":" << assertions_executed
               << ",\"real_audio_executed\":false,\"hardware_proven\":false,\"native_ready\":false,\"cases\":[";
        for (std::size_t i = 0; i < cases.size(); ++i) {
            if (i) stream << ',';
            stream << "{\"name\":\"" << cases[i].first << "\",\"status\":\"" << (cases[i].second ? "PASS" : "FAIL") << "\"}";
        }
        stream << "]}\n";
    };
    receipt(std::cout);
    if (argc == 3) {
        std::ofstream output(argv[2]);
        receipt(output);
        output.flush();
        if (!output) { std::cerr << "receipt write failed\n"; return 2; }
    }
    return failed ? 1 : 0;
}
