"""Streaming response example using the artemis-core model-centric API.

Demonstrates how to process token events as they arrive, giving you
streaming-style output without a separate stream method.
"""

import os

from artemis_core import ArtemisEngine, Message, Role


def main() -> None:
    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        print("Missing ANTHROPIC_API_KEY environment variable.")
        print("Set it before running: export ANTHROPIC_API_KEY=your-key")
        return

    engine = ArtemisEngine()
    engine.set_model("sonnet")

    messages = [
        Message(role=Role.User, content="Write a short poem about the sea."),
    ]

    # The event model handles streaming naturally.
    # Token events carry content fragments.
    events = engine.run_conversation(messages, [])

    print("Streaming response:\n")
    for event in events:
        if event.kind == "token" and event.content:
            # Print each token as it arrives.
            print(event.content, end="", flush=True)
        elif event.kind == "done":
            print(f"\n\nFinished: {event.finish_reason}")


if __name__ == "__main__":
    main()