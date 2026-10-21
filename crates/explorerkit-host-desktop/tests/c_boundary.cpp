#include "explorerkit_desktop_private.h"

#include <array>
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <cstdint>
#include <cstring>
#include <exception>
#include <limits>
#include <type_traits>
#include <vector>

namespace {

struct CallbackState {
  std::atomic<unsigned> wakes{0};
  unsigned events = 0;
};

void wake_callback(void *context, std::uint64_t token) noexcept {
  try {
    if (context == nullptr || token == 0) {
      std::terminate();
    }
    static_cast<CallbackState *>(context)->wakes.fetch_add(
        1, std::memory_order_relaxed);
  } catch (...) {
    std::terminate();
  }
}

void event_callback(void *context, std::uint64_t token,
                    std::uint64_t attachment_generation,
                    const char *event_json) noexcept {
  try {
    if (context == nullptr || token == 0 || attachment_generation == token ||
        event_json == nullptr || std::strlen(event_json) == 0) {
      std::terminate();
    }
    ++static_cast<CallbackState *>(context)->events;
  } catch (...) {
    std::terminate();
  }
}

static_assert(std::is_same_v<decltype(&wake_callback),
                             ExplorerKitDesktopPrivateWakeCallback>);
static_assert(std::is_same_v<decltype(&event_callback),
                             ExplorerKitDesktopPrivateEventCallback>);
static_assert(EXPLORERKIT_DESKTOP_PRIVATE_MAX_COMMAND_BYTES == 1024U * 1024U);

ExplorerKitDesktopPrivateInput input(std::uint32_t kind) {
  ExplorerKitDesktopPrivateInput value{};
  value.kind = kind;
  return value;
}

bool expect_status(const char *operation,
                   ExplorerKitDesktopPrivateStatus actual,
                   ExplorerKitDesktopPrivateStatus expected) {
  if (actual == expected) {
    return true;
  }
  std::fprintf(stderr, "%s: expected status %d, got %d\n", operation,
               static_cast<int>(expected), static_cast<int>(actual));
  return false;
}

bool expect_true(const char *operation, bool condition) {
  if (condition) {
    return true;
  }
  std::fprintf(stderr, "%s: check failed\n", operation);
  return false;
}

} // namespace

int main() {
  CallbackState state;
  ExplorerKitDesktopPrivateCallbacks callbacks{&state, wake_callback};
  ExplorerKitDesktopPrivateStatus status;

  status = explorerkit_desktop_private_create(callbacks, nullptr, nullptr);
  if (!expect_status("create rejects null outputs", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_NULL_POINTER)) {
    return EXIT_FAILURE;
  }

  ExplorerKitDesktopPrivateHost *host = nullptr;
  std::uint64_t token = 0;
  status = explorerkit_desktop_private_create(callbacks, &host, &token);
  if (!expect_status("create", status, EXPLORERKIT_DESKTOP_PRIVATE_OK) ||
      !expect_true("create returns a host", host != nullptr) ||
      !expect_true("create returns a nonzero token", token != 0)) {
    return EXIT_FAILURE;
  }

  status = explorerkit_desktop_private_pump(nullptr, token);
  if (!expect_status("pump rejects a null host", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_NULL_POINTER)) {
    return EXIT_FAILURE;
  }

  const std::uint64_t wrong_token =
      token == std::numeric_limits<std::uint64_t>::max() ? token - 1
                                                         : token + 1;
  status = explorerkit_desktop_private_pump(host, wrong_token);
  if (!expect_status("pump rejects a wrong token", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_STALE_TOKEN)) {
    return EXIT_FAILURE;
  }

  const ExplorerKitDesktopPrivateViewport viewport{0, 0, 320, 240, 2.0F};
  const ExplorerKitDesktopPrivateNativeSurface invalid_surface{nullptr, nullptr};
  std::uint64_t attachment_generation =
      std::numeric_limits<std::uint64_t>::max();
  status = explorerkit_desktop_private_attach(host, token, invalid_surface,
                                           viewport, &attachment_generation);
  if (!expect_status("attach rejects a null native handle", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT) ||
      !expect_true("failed attach clears its generation output",
                   attachment_generation == 0)) {
    return EXIT_FAILURE;
  }

  constexpr char reload_json[] = "{\"version\":1,\"command\":\"reload\"}";
  std::array<std::uint8_t, sizeof(reload_json) - 1> reload_command{};
  std::memcpy(reload_command.data(), reload_json, reload_command.size());

  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, nullptr, 1);
  if (!expect_status("command dispatch rejects a null pointer", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT)) {
    return EXIT_FAILURE;
  }
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, reload_command.data(), 0);
  if (!expect_status("command dispatch rejects zero bytes", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT)) {
    return EXIT_FAILURE;
  }

  std::vector<std::uint8_t> oversized_command(
      EXPLORERKIT_DESKTOP_PRIVATE_MAX_COMMAND_BYTES + 1, ' ');
  std::memcpy(oversized_command.data(), reload_json, reload_command.size());
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, oversized_command.data(), oversized_command.size());
  if (!expect_status("command dispatch rejects oversized input", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT)) {
    return EXIT_FAILURE;
  }

  constexpr std::uint8_t invalid_utf8[]{0xFF};
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, invalid_utf8, sizeof(invalid_utf8));
  if (!expect_status("command dispatch rejects invalid UTF-8", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT)) {
    return EXIT_FAILURE;
  }

  constexpr std::uint8_t invalid_json[]{'{'};
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, invalid_json, sizeof(invalid_json));
  if (!expect_status("Rust rejects invalid command JSON", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT)) {
    return EXIT_FAILURE;
  }

  status = explorerkit_desktop_private_dispatch_controller_command(
      host, wrong_token, reload_command.data(), reload_command.size());
  if (!expect_status("command dispatch rejects a wrong token", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_STALE_TOKEN)) {
    return EXIT_FAILURE;
  }

  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, reload_command.data(), reload_command.size());
  if (!expect_status("command dispatch accepts an exact non-NUL slice", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  constexpr char stop_command[] = "{\"version\":1,\"command\":\"stopLoading\"}";
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, reinterpret_cast<const std::uint8_t *>(stop_command),
      sizeof(stop_command) - 1);
  if (!expect_status("unsupported Stop rejects before the detached queue", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_RUNTIME_ERROR)) {
    return EXIT_FAILURE;
  }

  constexpr char command_with_trailing_bytes[] =
      "{\"version\":1,\"command\":\"reload\"}not part of the command";
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token,
      reinterpret_cast<const std::uint8_t *>(command_with_trailing_bytes),
      reload_command.size());
  if (!expect_status("command dispatch excludes bytes after the length", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  constexpr char invalid_envelope[] =
      "{\"version\":2,\"command\":\"reload\"}";
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, reinterpret_cast<const std::uint8_t *>(invalid_envelope),
      sizeof(invalid_envelope) - 1);
  if (!expect_status("Rust rejects an invalid command envelope", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT)) {
    return EXIT_FAILURE;
  }

  constexpr char load_command[] =
      "{\"version\":1,\"command\":\"loadUrl\",\"url\":\"example.com\"}";
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, reinterpret_cast<const std::uint8_t *>(load_command),
      sizeof(load_command) - 1);
  if (!expect_status("queue detached load command", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  status = explorerkit_desktop_private_update_viewport(host, token, viewport);
  if (!expect_status("detached viewport update", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_RUNTIME_ERROR)) {
    return EXIT_FAILURE;
  }

  constexpr char evaluation_command[] =
      "{\"version\":1,\"command\":\"evaluateJavaScript\","
      "\"evaluationId\":\"detached-evaluation\",\"script\":\"1 + 1\"}";
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token, reinterpret_cast<const std::uint8_t *>(evaluation_command),
      sizeof(evaluation_command) - 1);
  if (!expect_status("queue detached JavaScript", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  constexpr char navigation_response_command[] =
      "{\"version\":1,\"command\":\"resolveNavigationRequest\","
      "\"navigationId\":\"navigation-1\",\"allow\":true}";
  status = explorerkit_desktop_private_dispatch_controller_command(
      host, token,
      reinterpret_cast<const std::uint8_t *>(navigation_response_command),
      sizeof(navigation_response_command) - 1);
  if (!expect_status("generic dispatch accepts a navigation response", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  auto pointer = input(EXPLORERKIT_DESKTOP_PRIVATE_INPUT_POINTER_BUTTON);
  pointer.button_kind = EXPLORERKIT_DESKTOP_PRIVATE_BUTTON_OTHER;
  pointer.button_code = UINT16_MAX;
  pointer.action = EXPLORERKIT_DESKTOP_PRIVATE_PRESSED;
  status = explorerkit_desktop_private_dispatch_input(host, token, pointer);
  if (!expect_status("accept detached pointer input", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  auto keyboard = input(EXPLORERKIT_DESKTOP_PRIVATE_INPUT_KEYBOARD);
  keyboard.key_kind = EXPLORERKIT_DESKTOP_PRIVATE_KEY_CHARACTER;
  keyboard.text = "Enter";
  keyboard.action = EXPLORERKIT_DESKTOP_PRIVATE_PRESSED;
  status = explorerkit_desktop_private_dispatch_input(host, token, keyboard);
  if (!expect_status("accept detached keyboard input", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  status = explorerkit_desktop_private_detach(host, token);
  if (!expect_status("first detached detach", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }
  status = explorerkit_desktop_private_detach(host, token);
  if (!expect_status("second detached detach", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }

  status = explorerkit_desktop_private_drain_events(host, token, event_callback,
                                                 &state);
  if (!expect_status("drain events", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_OK) ||
      !expect_true("detached controller has no events", state.events == 0)) {
    return EXIT_FAILURE;
  }
  status =
      explorerkit_desktop_private_drain_events(host, token, nullptr, nullptr);
  if (!expect_status("drain rejects a null callback", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT)) {
    return EXIT_FAILURE;
  }

  status = explorerkit_desktop_private_destroy(host, token);
  if (!expect_status("destroy", status, EXPLORERKIT_DESKTOP_PRIVATE_OK)) {
    return EXIT_FAILURE;
  }
  status = explorerkit_desktop_private_pump(host, token);
  if (!expect_status("destroy makes the token stale", status,
                     EXPLORERKIT_DESKTOP_PRIVATE_STALE_TOKEN)) {
    return EXIT_FAILURE;
  }
  return EXIT_SUCCESS;
}
