// The YGLogger trampoline of opencode-yoga (see src/lib.rs, `Config::set_logger`): formats a Yoga message into a
// buffer of OT_YOGA_LOG_BUFFER_SIZE (4096) bytes as yoga-bridge.cpp's otYogaFormatLog does for yoga.zig's
// internalLogger, and hands it to Rust. A va_list cannot cross into Rust portably, so formatting stays on this side.
#include <yoga/Yoga.h>

#include <cstdarg>
#include <cstdint>
#include <cstdio>

extern "C" int opencodeYogaLogMessage(YGConfigConstRef config, int level, const uint8_t* message,
                                      int length) noexcept;

extern "C" int opencodeYogaLogger(YGConfigConstRef config, YGNodeConstRef, YGLogLevel level,
                                  const char* format, va_list args) noexcept {
  uint8_t message[4096];
  int length;
  try {
    length = std::vsnprintf(reinterpret_cast<char*>(message), sizeof message, format, args);
  } catch (...) {
    length = -1;
  }
  return opencodeYogaLogMessage(config, static_cast<int>(level), message, length);
}
