#include "FileIDORTC.h"

#include <onnxruntime/onnxruntime_c_api.h>

bool fileid_ort_disable_telemetry_events(void) {
  const OrtApi *api = OrtGetApiBase()->GetApi(ORT_API_VERSION);
  if (api == NULL) return false;
  OrtStatus *status = api->DisableTelemetryEvents(NULL);
  if (status == NULL) return true;
  api->ReleaseStatus(status);
  return false;
}
