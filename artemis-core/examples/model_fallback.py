"""Model fallback example using the artemis-core model-centric API.

Demonstrates how to try a primary model and fall back to a secondary
one if it fails. The engine resolves providers automatically, so
switching models is a single call.
"""

import os

from artemis_core import ArtemisEngine, Message, Role


def try_model(engine: ArtemisEngine, model_name: str, messages: list) -> str | None:
    """Try running a conversation with a given model. Returns content or None."""
    try:
        engine.set_model(model_name)
        events = engine.run_conversation(messages, [])
        content_parts = []
        for event in events:
            if event.kind == "token" and event.content:
                content_parts.append(event.content)
            elif event.kind == "done":
                if event.finish_reason == "stop":
                    return "".join(content_parts)
        return None
    except RuntimeError as e:
        print(f"Model '{model_name}' failed: {e}")
        return None


def main() -> None:
    api_key = os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("OPENAI_API_KEY")
    if not api_key:
        print("Missing API key. Set ANTHROPIC_API_KEY or OPENAI_API_KEY.")
        return

    engine = ArtemisEngine()
    messages = [Message(role=Role.User, content="Explain quantum computing briefly.")]

    # Try primary model first.
    primary = "sonnet"
    fallback = "gpt-4o"

    print(f"Trying primary model: {primary}")
    result = try_model(engine, primary, messages)

    if result is None:
        print(f"Primary failed. Falling back to: {fallback}")
        result = try_model(engine, fallback, messages)

    if result:
        print(f"\nResponse:\n{result}")
    else:
        print("All models failed.")


if __name__ == "__main__":
    main()