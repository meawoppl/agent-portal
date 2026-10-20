# Antigravity support

Antigravity is an experimental, read-only Agent Portal backend. The launcher
runs `localharness` through the typed `antigravity-codes` SDK. Agent selection,
scheduling, installation, model selection, transcript rendering and usage
metrics are integrated with the existing session machinery.

## Runtime and identity

Each turn launches the harness with the Portal session UUID as its cascade ID
and persistent storage under the user's Portal data directory. Before sending
input, Portal verifies that the harness returned that exact ID. Missing or
mismatched IDs fail delivery rather than attaching input to another conversation.
Replayed initialization history warms the step assembler without duplicating
transcript rows. Active steps are ephemeral; completed steps are durable.
The wire envelopes are `antigravity_event` and `antigravity_turn_completed`.

At the end of a turn, Portal asks the SDK to shut down the harness. This is
bounded to five seconds, inside the agent's eight-second graceful-stop window.
A timeout or SDK error emits a visible error and marks normal turn completion
as unsuccessful. Closing the command channel stops queued work from launching
another harness. Shutdown success is the SDK's result, not an independent
on-disk persistence audit: the SDK currently tolerates a closed stream while
waiting for its session-end acknowledgement.

## Installation and authentication

The Computers tab installs the PyPI harness in a launcher-owned virtual
environment. Discovery also supports `ANTIGRAVITY_HARNESS_PATH`, `localharness`
on `PATH`, and the binary in an independently installed Python wheel.

The default model is `gemini-flash-latest`; the model selector and `--model`
argument configure other supported models. Credential probing recognizes
Gemini API-key configuration and Vertex configuration. This checks local
configuration presence; it does not prove that a provider will accept the
credentials. Configure credentials on the machine running the launcher.

## Safety boundary

The preview keeps the existing read-only policy: write operations and
client-side tool execution are denied. Portal permission controls do not
convert this preview into a write-capable agent. Provider-side behavior is
still subject to the SDK and harness policy implementation.

## Verification

`cargo test -p antigravity-session-lib` covers identity mismatch/missing IDs,
shutdown success/error/timeout, model and policy configuration, replay assembly
and metrics. Integration requires an installed harness and provider credentials;
unit tests do not spend provider credits or claim a live-provider smoke test.
