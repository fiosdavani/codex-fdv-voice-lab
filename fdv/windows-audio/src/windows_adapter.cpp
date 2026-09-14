#include "windows_audio.hpp"
#include <windows.h>
#include <audioclient.h>
#include <audiopolicy.h>
#include <mmdeviceapi.h>
#include <tlhelp32.h>
#include <ks.h>
#include <mmreg.h>
#include <ksmedia.h>
#include <wrl/client.h>
#include <algorithm>
#include <atomic>
#include <cstring>
#include <stdexcept>
#include <utility>

namespace vai {
using Microsoft::WRL::ComPtr;
namespace {
void check(HRESULT hr, const char* operation) {
    if (FAILED(hr)) throw std::runtime_error(operation);
}
class Apartment {
    DWORD thread_{GetCurrentThreadId()};
public:
    Apartment() { check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "COM_MTA_FAILED"); }
    ~Apartment() { if (thread_ == GetCurrentThreadId()) CoUninitialize(); }
    Apartment(const Apartment&) = delete;
    Apartment& operator=(const Apartment&) = delete;
};
struct Handle {
    HANDLE h{INVALID_HANDLE_VALUE};
    explicit Handle(HANDLE value) : h(value) {}
    ~Handle() { if (h && h != INVALID_HANDLE_VALUE) CloseHandle(h); }
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
};
std::optional<ProcessIdentity> process_identity(DWORD pid) {
    Handle handle(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid));
    if (!handle.h) return std::nullopt;
    FILETIME creation{}, exit_time{}, kernel{}, user{};
    DWORD length = 32768;
    std::wstring path(length, L'\0');
    if (!GetProcessTimes(handle.h, &creation, &exit_time, &kernel, &user) ||
        !QueryFullProcessImageNameW(handle.h, 0, path.data(), &length)) return std::nullopt;
    path.resize(length);
    return ProcessIdentity{pid, (static_cast<std::uint64_t>(creation.dwHighDateTime) << 32) |
                               creation.dwLowDateTime, path};
}
bool same_tree(const ProcessTree& a, const ProcessTree& b) {
    if (!a.complete || !b.complete || a.members.size() != b.members.size()) return false;
    for (const auto& member : a.members)
        if (std::find(b.members.begin(), b.members.end(), member) == b.members.end()) return false;
    return true;
}
// Each callback performs only a nonblocking atomic invalidation. It never mutes,
// unregisters, waits, or releases a final audio reference on the callback thread.
class Invalidations final : public IMMNotificationClient, public IAudioSessionNotification,
                            public IAudioSessionEvents {
    std::atomic<ULONG> refs_{1};
public:
    std::atomic<std::uint64_t> revision{1};
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** out) override {
        if (!out) return E_POINTER;
        *out = nullptr;
        if (id == __uuidof(IUnknown) || id == __uuidof(IMMNotificationClient))
            *out = static_cast<IMMNotificationClient*>(this);
        else if (id == __uuidof(IAudioSessionNotification)) *out = static_cast<IAudioSessionNotification*>(this);
        else if (id == __uuidof(IAudioSessionEvents)) *out = static_cast<IAudioSessionEvents*>(this);
        else return E_NOINTERFACE;
        AddRef(); return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++refs_; }
    ULONG STDMETHODCALLTYPE Release() override {
        const auto remaining = --refs_;
        if (!remaining) delete this;
        return remaining;
    }
    HRESULT dirty() { revision.fetch_add(1); return S_OK; }
    HRESULT STDMETHODCALLTYPE OnDeviceStateChanged(LPCWSTR, DWORD) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnDeviceAdded(LPCWSTR) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnDeviceRemoved(LPCWSTR) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnDefaultDeviceChanged(EDataFlow, ERole, LPCWSTR) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnPropertyValueChanged(LPCWSTR, const PROPERTYKEY) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnSessionCreated(IAudioSessionControl*) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnDisplayNameChanged(LPCWSTR, LPCGUID) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnIconPathChanged(LPCWSTR, LPCGUID) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnSimpleVolumeChanged(float, BOOL mute, LPCGUID) override {
        // TRUE cannot make an unsafe session audible. FALSE always invalidates.
        return mute ? S_OK : dirty();
    }
    HRESULT STDMETHODCALLTYPE OnChannelVolumeChanged(DWORD, float[], DWORD, LPCGUID) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnGroupingParamChanged(LPCGUID, LPCGUID) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnStateChanged(AudioSessionState) override { return dirty(); }
    HRESULT STDMETHODCALLTYPE OnSessionDisconnected(AudioSessionDisconnectReason) override { return dirty(); }
};
struct Registration {
    ComPtr<IAudioSessionManager2> manager;
    ComPtr<Invalidations> events;
    ~Registration() { if (manager && events) manager->UnregisterSessionNotification(events.Get()); }
};
struct BoundSession {
    AudioSession view;
    ComPtr<IAudioSessionControl2> control;
    ComPtr<ISimpleAudioVolume> volume;
    ComPtr<Invalidations> events;
    ~BoundSession() { if (control && events) control->UnregisterAudioSessionNotification(events.Get()); }
};
class WindowsRender final : public IRenderPlatform {
    Apartment apartment_;
    ProcessIdentity root_;
    ProcessTree tree_;
    ComPtr<IMMDeviceEnumerator> devices_;
    ComPtr<Invalidations> events_;
    std::vector<std::unique_ptr<Registration>> managers_;
    std::vector<std::unique_ptr<BoundSession>> sessions_;
    bool registered_{};
    bool complete_{true};
    std::uint64_t initial_revision_{};
    BoundSession* find(const SessionIdentity& id) {
        for (auto& session : sessions_) if (session->view.identity == id) return session.get();
        return nullptr;
    }
    bool still_bound(const SessionIdentity& id) {
        const auto live = process_identity(id.process.pid);
        if (!live || !(*live == id.process)) return false;
        auto* s = find(id);
        if (!s) return false;
        DWORD pid{};
        return s->control->GetProcessId(&pid) == S_OK && pid == id.process.pid;
    }
public:
    explicit WindowsRender(ProcessIdentity root) : root_(std::move(root)), tree_(discover_process_tree(root_)) {
        if (!tree_.complete) throw std::runtime_error("PROCESS_TREE_INCOMPLETE");
        events_.Attach(new Invalidations());
        check(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
                               IID_PPV_ARGS(&devices_)), "ENUMERATOR_FAILED");
        check(devices_->RegisterEndpointNotificationCallback(events_.Get()), "ENDPOINT_NOTIFY_FAILED");
        registered_ = true;
        // Capture baseline BEFORE enumerating so any concurrent creation fails.
        initial_revision_ = events_->revision.load();
        try {
            ComPtr<IMMDeviceCollection> collection;
            check(devices_->EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE, &collection), "RENDER_ENUM_FAILED");
            UINT count{};
            check(collection->GetCount(&count), "ENDPOINT_COUNT_FAILED");
            for (UINT i = 0; i < count; ++i) {
                ComPtr<IMMDevice> endpoint;
                check(collection->Item(i, &endpoint), "ENDPOINT_ITEM_FAILED");
                LPWSTR raw_endpoint{};
                check(endpoint->GetId(&raw_endpoint), "ENDPOINT_ID_FAILED");
                const std::wstring endpoint_id(raw_endpoint);
                CoTaskMemFree(raw_endpoint);
                auto registration = std::make_unique<Registration>();
                check(endpoint->Activate(__uuidof(IAudioSessionManager2), CLSCTX_INPROC_SERVER, nullptr,
                                         reinterpret_cast<void**>(registration->manager.GetAddressOf())), "MANAGER_FAILED");
                check(registration->manager->RegisterSessionNotification(events_.Get()), "SESSION_NOTIFY_FAILED");
                registration->events = events_;
                ComPtr<IAudioSessionEnumerator> enumerator;
                check(registration->manager->GetSessionEnumerator(&enumerator), "SESSION_ENUM_FAILED");
                int session_count{};
                // Required by Microsoft before new-session callbacks start.
                check(enumerator->GetCount(&session_count), "SESSION_COUNT_FAILED");
                managers_.push_back(std::move(registration));
                for (int j = 0; j < session_count; ++j) {
                    ComPtr<IAudioSessionControl> control;
                    check(enumerator->GetSession(j, &control), "SESSION_ITEM_FAILED");
                    auto session = std::make_unique<BoundSession>();
                    check(control.As(&session->control), "CONTROL2_FAILED");
                    check(control.As(&session->volume), "SIMPLE_VOLUME_FAILED");
                    check(session->control->RegisterAudioSessionNotification(events_.Get()), "STATE_NOTIFY_FAILED");
                    session->events = events_;
                    LPWSTR raw_instance{};
                    check(session->control->GetSessionInstanceIdentifier(&raw_instance), "SESSION_ID_FAILED");
                    session->view.identity.instance_id = raw_instance;
                    CoTaskMemFree(raw_instance);
                    session->view.identity.endpoint_id = endpoint_id;
                    session->view.render_only = true;
                    DWORD pid{};
                    const auto pid_result = session->control->GetProcessId(&pid);
                    check(pid_result, "SESSION_PID_FAILED");
                    // AUDCLNT_S_NO_SINGLE_PROCESS is success but is NOT a match.
                    session->view.single_process = pid_result == S_OK;
                    const auto system_result = session->control->IsSystemSoundsSession();
                    check(system_result, "SYSTEM_SESSION_FAILED");
                    session->view.system_sounds = system_result == S_OK;
                    session->view.identity.process.pid = pid;
                    if (const auto identity = process_identity(pid)) session->view.identity.process = *identity;
                    sessions_.push_back(std::move(session));
                }
            }
            complete_ = same_tree(tree_, discover_process_tree(root_));
        } catch (...) {
            devices_->UnregisterEndpointNotificationCallback(events_.Get());
            registered_ = false;
            throw;
        }
    }
    ~WindowsRender() override {
        if (registered_) devices_->UnregisterEndpointNotificationCallback(events_.Get());
        sessions_.clear();
        managers_.clear();
    }
    RenderSnapshot snapshot() override {
        const auto before = revision();
        RenderSnapshot out{before, complete_ && before == initial_revision_, {}};
        for (const auto& session : sessions_) out.sessions.push_back(session->view);
        return out;
    }
    bool mute(const SessionIdentity& id) override {
        auto* s = find(id);
        if (!s || !s->view.render_only || !s->view.single_process || s->view.system_sounds ||
            revision() != initial_revision_ || !still_bound(id)) return false;
        return s->volume->SetMute(TRUE, nullptr) == S_OK;
    }
    std::optional<bool> read_mute(const SessionIdentity& id) override {
        auto* s = find(id);
        if (!s || revision() != initial_revision_ || !still_bound(id)) return std::nullopt;
        BOOL is_muted{};
        if (s->volume->GetMute(&is_muted) != S_OK) return std::nullopt;
        return is_muted != FALSE;
    }
    std::uint64_t revision() const override {
        if (!same_tree(tree_, discover_process_tree(root_))) events_->dirty();
        return events_->revision.load();
    }
    bool prospective_render_barrier_held() const override { return false; }
};

class SharedMicrophone final : public IMicrophone {
    Apartment apartment_;
    ComPtr<IMMDeviceEnumerator> devices_;
    ComPtr<IAudioClient> client_;
    ComPtr<IAudioCaptureClient> capture_;
    ComPtr<Invalidations> events_;
    std::wstring endpoint_id_;
    std::uint64_t initial_revision_{};
    std::uint32_t sample_rate_{};
    std::uint16_t channels_{};
    bool float32_{};
    bool running_{};
    bool registered_{};
public:
    explicit SharedMicrophone(std::wstring endpoint_id) : endpoint_id_(std::move(endpoint_id)) {
        if (endpoint_id_.empty()) throw std::runtime_error("EXPLICIT_CAPTURE_ENDPOINT_REQUIRED");
        events_.Attach(new Invalidations());
        check(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
                               IID_PPV_ARGS(&devices_)), "ENUMERATOR_FAILED");
        check(devices_->RegisterEndpointNotificationCallback(events_.Get()), "ENDPOINT_NOTIFY_FAILED");
        registered_ = true;
        initial_revision_ = events_->revision.load();
        try {
            ComPtr<IMMDevice> device;
            check(devices_->GetDevice(endpoint_id_.c_str(), &device), "CAPTURE_ENDPOINT_MISSING");
            ComPtr<IMMEndpoint> endpoint;
            check(device.As(&endpoint), "ENDPOINT_INTERFACE_FAILED");
            EDataFlow direction{};
            check(endpoint->GetDataFlow(&direction), "ENDPOINT_DIRECTION_FAILED");
            if (direction != eCapture) throw std::runtime_error("MIC_REQUIRES_CAPTURE_ENDPOINT");
            check(device->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                                   reinterpret_cast<void**>(client_.GetAddressOf())), "CAPTURE_ACTIVATE_FAILED");
            WAVEFORMATEX* raw_format{};
            check(client_->GetMixFormat(&raw_format), "MIX_FORMAT_FAILED");
            const std::unique_ptr<WAVEFORMATEX, decltype(&CoTaskMemFree)> format(raw_format, CoTaskMemFree);
            sample_rate_ = format->nSamplesPerSec;
            channels_ = format->nChannels;
            WORD tag = format->wFormatTag;
            if (tag == WAVE_FORMAT_EXTENSIBLE && format->cbSize >= sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)) {
                const auto* extended = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(format.get());
                if (extended->SubFormat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT) tag = WAVE_FORMAT_IEEE_FLOAT;
                else if (extended->SubFormat == KSDATAFORMAT_SUBTYPE_PCM && extended->Samples.wValidBitsPerSample == 16)
                    tag = WAVE_FORMAT_PCM;
            }
            float32_ = tag == WAVE_FORMAT_IEEE_FLOAT && format->wBitsPerSample == 32;
            const bool pcm16 = tag == WAVE_FORMAT_PCM && format->wBitsPerSample == 16;
            if ((!float32_ && !pcm16) || !channels_ || channels_ > 8 || sample_rate_ < 8000 ||
                sample_rate_ > 192000 || format->nBlockAlign != channels_ * (float32_ ? 4 : 2))
                throw std::runtime_error("NC_CAPTURE_FORMAT_UNSUPPORTED");
            // Shared CAPTURE, no LOOPBACK, no exclusive takeover and no SetMute.
            check(client_->Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 1000000, 0, format.get(), nullptr),
                  "NC_SHARED_CAPTURE_INITIALIZE_FAILED");
            check(client_->GetService(IID_PPV_ARGS(&capture_)), "CAPTURE_SERVICE_FAILED");
            check(client_->Start(), "NC_SHARED_CAPTURE_START_FAILED");
            running_ = true;
        } catch (...) {
            devices_->UnregisterEndpointNotificationCallback(events_.Get());
            registered_ = false;
            throw;
        }
    }
    ~SharedMicrophone() override {
        if (running_) client_->Stop();
        if (registered_) devices_->UnregisterEndpointNotificationCallback(events_.Get());
    }
    std::optional<CapturePacket> next_packet() override {
        if (events_->revision.load() != initial_revision_) throw std::runtime_error("NC_ENDPOINT_CHANGED");
        UINT32 next{};
        check(capture_->GetNextPacketSize(&next), "NC_CAPTURE_PACKET_FAILED");
        if (!next) return std::nullopt;
        BYTE* data{};
        UINT32 frames{};
        DWORD flags{};
        UINT64 qpc{};
        const auto result = capture_->GetBuffer(&data, &frames, &flags, nullptr, &qpc);
        check(result, "NC_CAPTURE_BUFFER_FAILED");
        if (result == AUDCLNT_S_BUFFER_EMPTY || frames == 0) return std::nullopt;
        // RAII guarantees release on allocation/format errors, on this thread.
        struct PacketRelease {
            IAudioCaptureClient* client;
            UINT32 frames;
            ~PacketRelease() { if (frames) client->ReleaseBuffer(frames); }
        } release{capture_.Get(), frames};
        CapturePacket packet{endpoint_id_, initial_revision_, qpc, sample_rate_, channels_, {},
                             (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY) != 0,
                             (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR) != 0};
        const auto count = static_cast<std::size_t>(frames) * channels_;
        if (frames > sample_rate_) throw std::runtime_error("NC_CAPTURE_PACKET_OVERSIZE");
        packet.samples.resize(count, 0.0F);
        if (!(flags & AUDCLNT_BUFFERFLAGS_SILENT)) {
            if (!data) throw std::runtime_error("NC_CAPTURE_NULL_DATA");
            for (std::size_t i = 0; i < count; ++i) {
                if (float32_) std::memcpy(&packet.samples[i], data + i * sizeof(float), sizeof(float));
                else {
                    std::int16_t value{};
                    std::memcpy(&value, data + i * sizeof(value), sizeof(value));
                    packet.samples[i] = static_cast<float>(value) / 32768.0F;
                }
            }
        }
        const auto release_result = capture_->ReleaseBuffer(frames);
        release.frames = 0;
        check(release_result, "NC_CAPTURE_RELEASE_FAILED");
        if (events_->revision.load() != initial_revision_) throw std::runtime_error("NC_ENDPOINT_CHANGED");
        return packet;
    }
};
} // namespace

ProcessTree discover_process_tree(const ProcessIdentity& root) {
    ProcessTree result;
    const auto live_root = process_identity(root.pid);
    if (!live_root || !(*live_root == root)) return result;
    Handle snapshot(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
    if (snapshot.h == INVALID_HANDLE_VALUE) return result;
    std::vector<PROCESSENTRY32W> processes;
    PROCESSENTRY32W entry{};
    entry.dwSize = static_cast<DWORD>(sizeof(entry));
    if (!Process32FirstW(snapshot.h, &entry)) return result;
    do { processes.push_back(entry); } while (Process32NextW(snapshot.h, &entry));
    if (GetLastError() != ERROR_NO_MORE_FILES) return result;
    result.members.push_back(root);
    // Includes renderer/GPU/audio utility grandchildren. Unknown paths remain
    // candidates for external review, never automatically authorized targets.
    for (std::size_t i = 0; i < result.members.size(); ++i) {
        const auto parent = result.members[i];
        for (const auto& candidate : processes) {
            if (candidate.th32ParentProcessID != parent.pid || candidate.th32ProcessID == parent.pid) continue;
            const auto child = process_identity(candidate.th32ProcessID);
            if (!child || child->creation_time_100ns < parent.creation_time_100ns) return result;
            if (std::find(result.members.begin(), result.members.end(), *child) == result.members.end())
                result.members.push_back(*child);
        }
    }
    const auto root_after = process_identity(root.pid);
    result.complete = root_after && *root_after == root;
    return result;
}
std::unique_ptr<IRenderPlatform> create_render_adapter(const ProcessIdentity& root) {
    return std::make_unique<WindowsRender>(root);
}
std::unique_ptr<IMicrophone> open_shared_microphone(const std::wstring& endpoint) {
    return std::make_unique<SharedMicrophone>(endpoint);
}
const char* native_capabilities() {
    return R"({"native_ready":false,"A":"NC_FUTURE_SESSION_PRE_RENDER_BARRIER","B":"NC_SCOPE_AND_ACOUSTIC_PROOF","real_audio_executed":false,"capture":"shared_WASAPI_candidate","authority":"external_provider_missing"})";
}
} // namespace vai
