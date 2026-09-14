#pragma once
#include "audio_policy.hpp"

namespace vai {
struct ProcessTree {
    bool complete{};
    std::vector<ProcessIdentity> members;
};
// Inventory ONLY. Process identity and ancestry do not grant voice authority.
ProcessTree discover_process_tree(const ProcessIdentity& expected_root);
// All objects must be created/used/destroyed on one dedicated MTA worker.
// These factories invoke real Windows APIs. No executable in this candidate
// calls them. A later authorized host must provide scope and session grants.
std::unique_ptr<IRenderPlatform> create_render_adapter(const ProcessIdentity& expected_root);
std::unique_ptr<IMicrophone> open_shared_microphone(const std::wstring& exact_capture_endpoint);
const char* native_capabilities();
}
