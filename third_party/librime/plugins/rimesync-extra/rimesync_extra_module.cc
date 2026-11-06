// rimesync-extra module: registers the bundled components.

#include <rime/common.h>
#include <rime/component.h>
#include <rime/registry.h>
#include <rime_api.h>

#include "numpad_processor.h"
#include "punct_processor.h"
#include "vnum_translator.h"
#include "vselect_processor.h"

using namespace rime;

static void rime_rimesync_extra_initialize() {
  LOG(INFO) << "registering components from module 'rimesync_extra'.";
  Registry& r = Registry::instance();
  r.Register("rimesync_vnum", new Component<rimesync::VnumTranslator>);
  r.Register("rimesync_punct", new Component<rimesync::PunctProcessor>);
  r.Register("rimesync_vselect", new Component<rimesync::VselectProcessor>);
  r.Register("numpad", new Component<rimesync::NumpadProcessor>);
}

static void rime_rimesync_extra_finalize() {}

RIME_REGISTER_MODULE(rimesync_extra)
