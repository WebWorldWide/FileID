#pragma once

#include <stdbool.h>

/// Calls ONNX Runtime's DisableTelemetryEvents, which the Objective-C bindings
/// don't expose. It acts on the process-wide provider, so no OrtEnv is needed.
bool fileid_ort_disable_telemetry_events(void);
