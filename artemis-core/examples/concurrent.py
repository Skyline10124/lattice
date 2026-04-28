"""Concurrent conversations example using the artemis-core model-centric API.

Demonstrates running multiple conversations with different models
using separate ArtemisEngine instances. Each engine holds its own
conversation state.
"""

import os
from concurrent.futures import ThreadPoolExecutor, as_completed

from artemis_core import ArtemisEngine, Message, Role


def run_conversation(model: str, prompt: str) -> tuple[str, str]:
    """Run a single conversation and return (model, response_content)."""
    engine = ArtemisEngine()
    engine.set_model(model)

    messages = [Message(role=Role.User, content=prompt)]
    events = engine.run_conversation(messages, [])

    parts: list[str] = []
    for event in events:
        if event.kind == "token" and event.content:
            parts.append(event.content)
    return (model, "".join(parts))


def main() -> None:
    api_key = os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("OPENAI_API_KEY")
    if not api_key:
        print("Missing API key. Set ANTHROPIC_API_KEY or OPENAI_API_KEY.")
        return

    # Define concurrent tasks: each with a different model and prompt.
    tasks = [
        ("sonnet", "Explain recursion in one paragraph."),
        ("gpt-4o", "What is the difference between a list and a tuple?"),
        ("sonnet", "Give me a haiku about programming."),
    ]

    print(f"Running {len(tasks)} concurrent conversations...\n")

    with ThreadPoolExecutor(max_workers=3) as pool:
        futures = {
            pool.submit(run_conversation, model, prompt): (model, prompt)
            for model, prompt in tasks
        }

        for future in as_completed(futures):
            model, prompt = futures[future]
            try:
                result_model, content = future.result()
                print(f"[{result_model}] Q: {prompt}")
                print(f"[{result_model}] A: {content}\n")
            except RuntimeError as e:
                print(f"[{model}] Error: {e}\n")


if __name__ == "__main__":
    main()