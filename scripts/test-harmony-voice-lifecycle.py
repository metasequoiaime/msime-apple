#!/usr/bin/env python3
"""Guard Harmony voice and panel creation against teardown races."""

from pathlib import Path


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    voice = (root / "platforms/harmony/entry/src/main/ets/keyboard/input/HarmonyVoiceRecognizer.ets").read_text()
    behaviour = (root / "platforms/harmony/entry/src/main/ets/keyboard/input/HarmonyVoiceRecordingBehaviour.ets").read_text()
    ability = (root / "platforms/harmony/entry/src/main/ets/inputmethodextability/KeyboardExtensionAbility.ets").read_text()

    checks = {
        "voice creation uses a local engine":
            "const created: speechRecognizer.SpeechRecognitionEngine =" in voice,
        "voice creation rejects a cancelled session":
            "if (session !== this.sessionId || this.resultHandler === undefined)" in voice,
        "voice creation releases a late engine":
            "created.shutdown();" in voice,
        "voice errors stop the write-audio capture":
            "const handler: VoiceErrorHandler | undefined = this.errorHandler;" in voice
            and "this.cancelSystemSession();" in voice
            and "this.resultHandler = undefined;" in voice
            and "this.errorHandler = undefined;" in voice,
        "voice tone closes raw file after player creation failure":
            "let rawFdOpen: boolean = false;" in behaviour
            and "rawFdOpen = true;" in behaviour
            and "if (rawFdOpen)" in behaviour
            and "await context.resourceManager.closeRawFd(asset);" in behaviour,
        "main panel creation checks teardown":
            "const panel: inputMethodEngine.Panel = await engine.createPanel" in ability
            and "if (this.tornDown) {" in ability,
        "late main panel is destroyed":
            "await engine.destroyPanel(panel);" in ability,
        "toolbar creation checks teardown":
            "const toolbar: inputMethodEngine.Panel = await engine.createPanel" in ability
            and "await engine.destroyPanel(toolbar);" in ability,
    }
    problems = [name for name, present in checks.items() if not present]
    if problems:
        for problem in problems:
            print(f"missing Harmony lifecycle guard: {problem}")
        return 1
    print("harmony voice lifecycle: late engines and panels are released after teardown")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
