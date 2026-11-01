// rimesync-extra: "v" mode numeric translator.

#include "vnum_translator.h"

#include <string>

#include <rime/candidate.h>
#include <rime/segmentation.h>
#include <rime/translation.h>

#include "vnum_convert.h"

namespace rimesync {

using namespace rime;

namespace {
const char* const kComponentName = "rimesync_vnum";
}  // namespace

VnumTranslator::VnumTranslator(const Ticket& ticket) : Translator(ticket) {}

an<Translation> VnumTranslator::Query(const string& input,
                                     const Segment& segment) {
  if (segment.end > input.size() || segment.start >= segment.end)
    return nullptr;
  const string text = input.substr(segment.start, segment.end - segment.start);
  // "v" + at least one digit.
  if (text.size() < 2 || (text[0] != 'v' && text[0] != 'V'))
    return nullptr;

  const string number = text.substr(1);
  VnumValue value;
  if (!ParseVnum(number, &value))
    return nullptr;
  int64_t yuan = 0;
  int fen = 0;
  if (!VnumToAmount(value, &yuan, &fen))
    return nullptr;

  const string upper = to_upper_amount(yuan, fen);
  const string lower = to_lower_chinese(number);

  auto translation = New<FifoTranslation>();
  translation->Append(New<SimpleCandidate>(kComponentName, segment.start,
                                           segment.end, upper, u8"大写"));
  if (!lower.empty() && lower != upper) {
    translation->Append(New<SimpleCandidate>(kComponentName, segment.start,
                                             segment.end, lower, u8"小写"));
  }
  return translation;
}

}  // namespace rimesync
