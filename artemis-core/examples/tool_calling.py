"""Tool calling example using the artemis-core model-centric API.

Demonstrates the full tool call roundtrip: define tools, receive a
tool_call_required event, execute the tool locally, and submit results
back to the engine for a followup response.
"""

import json
import os

from artemis_core import ArtemisEngine, Message, Role, ToolDefinition


def get_weather(city: str) -> str:
    """Fake weather tool for demonstration."""
    # In a real app, this would call an actual weather API.
    temps = {"Paris": "18°C", "Tokyo": "22°C", "New York": "15°C"}
    return temps.get(city, "20°C (default)")


def main() -> None:
    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        print("Missing ANTHROPIC_API_KEY environment variable.")
        print("Set it before running: export ANTHROPIC_API_KEY=your-key")
        return

    engine = ArtemisEngine()
    engine.set_model("sonnet")

    # Define a tool the model can call.
    # ToolDefinition takes name, description, and a JSON schema for parameters.
    weather_tool = ToolDefinition(
        name="get_weather",
        description="Get current weather for a city",
        parameters='{"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}',
    )

    messages = [
        Message(role=Role.User, content="What's the weather in Paris?"),
    ]

    # First turn: the model may request a tool call.
    events = engine.run_conversation(messages, [weather_tool])

    tool_results: list[tuple[str, str]] = []

    for event in events:
        if event.kind == "token" and event.content:
            print(f"Model says: {event.content}")
        elif event.kind == "tool_call_required" and event.tool_calls:
            for tc in event.tool_calls:
                print(f"Tool call: {tc.name}({tc.arguments})")
                args = json.loads(tc.arguments)
                result = get_weather(args.get("city", ""))
                print(f"Tool result: {result}")
                tool_results.append((tc.id, result))
        elif event.kind == "done":
            print(f"Turn finished: {event.finish_reason}")

    # Second turn: submit tool results and get the final answer.
    if tool_results:
        print("\nSubmitting tool results...")
        followup_events = engine.submit_tool_results(tool_results)
        for event in followup_events:
            if event.kind == "token" and event.content:
                print(event.content)
            elif event.kind == "done":
                print(f"\nFinal: {event.finish_reason}")


if __name__ == "__main__":
    main()