"""Basic chat example using the artemis-core model-centric API.

Demonstrates the simplest way to start a conversation with ArtemisEngine.
You pick a model, not a provider. The engine resolves everything else.
"""

import os

from artemis_core import ArtemisEngine, Message, Role


def main() -> None:
    # Check that an API key is available.
    # The engine auto-resolves credentials from environment variables.
    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        print("Missing ANTHROPIC_API_KEY environment variable.")
        print("Set it before running: export ANTHROPIC_API_KEY=your-key")
        return

    engine = ArtemisEngine()

    # Model-centric: just say which model you want.
    # The engine figures out the provider and credentials.
    engine.set_model("sonnet")

    # Check which model we're using (resolved canonical ID).
    model = engine.get_model()
    print(f"Active model: {model}")

    # Build a simple message list.
    messages = [
        Message(role=Role.User, content="What is the capital of France?"),
    ]

    # Run the conversation. Pass [] for tools if you don't need them.
    events = engine.run_conversation(messages, [])

    # Process events to extract the response.
    for event in events:
        if event.kind == "token" and event.content:
            print(event.content)
        elif event.kind == "done":
            print(f"\nFinished: {event.finish_reason}")


if __name__ == "__main__":
    main()