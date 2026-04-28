"""Custom model registration example using the artemis-core model-centric API.

Demonstrates runtime registration of a custom model with register_model().
No config file edits or restart required.
"""

import os

from artemis_core import ArtemisEngine, Message, Role


def main() -> None:
    # For a custom provider, you need credentials in whatever format
    # that provider expects. Here we use a generic API key env var.
    api_key = os.environ.get("MY_CUSTOM_API_KEY")
    if not api_key:
        print("Missing MY_CUSTOM_API_KEY environment variable.")
        print("This example uses a fictional provider for demonstration.")
        print("In practice, set the relevant env var for your custom provider.")

    engine = ArtemisEngine()

    # Register a custom model at runtime.
    # No config.yaml. No restart. Available immediately.
    engine.register_model(
        canonical_id="custom-vision-v1",
        display_name="Custom Vision V1",
        provider_id="my-custom-provider",
        api_model_id="vision-v1",
        base_url="https://api.mycompany.com/v1",
        api_protocol_str="chat_completions",
    )

    # Verify it's registered.
    models = engine.list_models()
    print(f"Registered models: {models}")

    # Resolve it to see full details.
    resolved = engine.resolve_model("custom-vision-v1")
    print(f"Resolved model:")
    print(f"  canonical_id: {resolved.canonical_id}")
    print(f"  provider:     {resolved.provider}")
    print(f"  base_url:     {resolved.base_url}")
    print(f"  api_protocol: {resolved.api_protocol}")

    # Set it as the active model.
    engine.set_model("custom-vision-v1")

    # Use it like any other model.
    messages = [
        Message(role=Role.User, content="Describe what you see in this image."),
    ]

    try:
        events = engine.run_conversation(messages, [])
        for event in events:
            if event.kind == "token" and event.content:
                print(event.content)
            elif event.kind == "done":
                print(f"\nFinished: {event.finish_reason}")
    except RuntimeError as e:
        print(f"Could not reach custom provider: {e}")
        print("This is expected if the endpoint doesn't actually exist.")


if __name__ == "__main__":
    main()