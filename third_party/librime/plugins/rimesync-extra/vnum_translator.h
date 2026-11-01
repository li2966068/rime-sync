// rimesync-extra: "v" mode numeric translator (component "rimesync_vnum").
//
// Input "v1234.5" yields a financial upper-case candidate and a plain Chinese
// lower-case candidate. Only numbers are supported: no dates, no arithmetic.

#ifndef RIMESYNC_EXTRA_VNUM_TRANSLATOR_H_
#define RIMESYNC_EXTRA_VNUM_TRANSLATOR_H_

#include <rime/common.h>
#include <rime/translator.h>

namespace rimesync {

class VnumTranslator : public rime::Translator {
 public:
  explicit VnumTranslator(const rime::Ticket& ticket);

  rime::an<rime::Translation> Query(const rime::string& input,
                                    const rime::Segment& segment) override;
};

}  // namespace rimesync

#endif  // RIMESYNC_EXTRA_VNUM_TRANSLATOR_H_
