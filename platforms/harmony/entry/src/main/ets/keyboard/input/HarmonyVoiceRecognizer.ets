import { speechRecognizer } from '@kit.CoreSpeechKit';
import { VoiceRecognitionPolicy } from './VoiceRecognitionPolicy';
import { HarmonyDoubaoRecognizer } from './HarmonyDoubaoRecognizer';
import { HarmonyHttpAsrRecognizer } from './HarmonyHttpAsrRecognizer';
import { HarmonyVoicePolisher } from './HarmonyVoicePolisher';
import { VoiceInputConfiguration } from './VoiceInputConfiguration';
import { HttpAsrConfigurationPolicy } from './HttpAsrConfigurationPolicy';
import { HarmonyLocalAsrRecognizer } from './HarmonyLocalAsrRecognizer';
import { HarmonyPcmCapture } from './HarmonyPcmCapture';
import { LocalAsrPolicy, PcmFrameSlicer, SpeechSentenceAccumulator } from './LocalAsrPolicy';
import { KeyboardLog, describeError } from '../KeyboardLog';
import { VoiceSessionPolicy } from './VoiceSessionPolicy';

export type VoiceResultHandler = (text: string, final: boolean) => void;
export type VoiceErrorHandler = (message: string) => void;

/**
 * Harmony's native speech boundary. For the `system` provider the microphone is captured here and written to CoreSpeechKit in its write-audio mode (`recognitionMode: 0`); the `local` provider runs an installed sherpa-onnx model on the device. No raw microphone data is copied into the Engine, written to disk, or included in logs. The recognizer is recreated only after shutdown, and callbacks are accepted only for the active bounded session.
 */
export class HarmonyVoiceRecognizer {
  private engine: speechRecognizer.SpeechRecognitionEngine | undefined = undefined;
  private sessionId: string = '';
  private generation: number = 0;
  private resultHandler: VoiceResultHandler | undefined = undefined;
  private errorHandler: VoiceErrorHandler | undefined = undefined;
  private readonly doubao: HarmonyDoubaoRecognizer = new HarmonyDoubaoRecognizer();
  private readonly httpAsr: HarmonyHttpAsrRecognizer = new HarmonyHttpAsrRecognizer();
  private readonly polisher: HarmonyVoicePolisher = new HarmonyVoicePolisher();
  private readonly local: HarmonyLocalAsrRecognizer = new HarmonyLocalAsrRecognizer();
  private readonly capture: HarmonyPcmCapture = new HarmonyPcmCapture();
  private readonly slicer: PcmFrameSlicer = new PcmFrameSlicer();
  private readonly sentences: SpeechSentenceAccumulator = new SpeechSentenceAccumulator();
  private polishGeneration: number = 0;
  private lastResultText: string = '';
  private finalDelivered: boolean = false;

  /**
   * `hotwordOptions` are the prepared host options, used by the `local` provider to read the user's own words as hotwords; null skips them.
   */
  async start(language: string, config: VoiceInputConfiguration, onResult: VoiceResultHandler,
    onError: VoiceErrorHandler, hotwordOptions: Object | null = null): Promise<void> {
    const doubaoProvider: boolean = config.asr_provider === 'doubao'
      && config.asr_token.trim().length > 0;
    const httpProvider: boolean = HttpAsrConfigurationPolicy.supported(config.asr_provider);
    if (doubaoProvider && this.doubao.isActive()) throw new Error('豆包语音识别正在进行');
    if (httpProvider && this.httpAsr.isActive()) throw new Error('语音识别正在进行');
    const localProvider: boolean = config.asr_provider === 'local';
    if (localProvider && this.local.isActive()) throw new Error('语音识别正在进行');
    if (localProvider && !LocalAsrPolicy.usesLocalModel(config.asr_provider, config.asr_model_path)) {
      throw new Error('请先在设置中下载并选择本地语音模型');
    }
    // The recognizers all own the same microphone and the keyboard has one recording
    // session. Checking only the selected provider lets a configuration change start a
    // second provider while the first one is still uploading or decoding; `stop()` then
    // stops only the provider it happens to check first and leaves the other session alive.
    if (!VoiceSessionPolicy.canStart(this.doubao.isActive(), this.httpAsr.isActive(),
      this.local.isActive(), this.capture.isActive(),
      this.engine !== undefined && this.engine.isBusy())) {
      throw new Error('语音识别正在进行');
    }
    if (config.asr_provider !== 'doubao' && !httpProvider && !localProvider && config.asr_provider !== 'system'
      && config.asr_provider.length > 0) {
      throw new Error('当前 Harmony 尚未支持该云端语音服务');
    }
    if (this.engine !== undefined && this.engine.isBusy()) {
      throw new Error('语音识别正在进行');
    }
    this.resultHandler = onResult;
    this.errorHandler = onError;
    this.lastResultText = '';
    this.finalDelivered = false;
    this.polishGeneration += 1;
    const deliver: VoiceResultHandler = (text: string, final: boolean): void => {
      this.deliver(config, text, final);
    };
    if (doubaoProvider) {
      await this.doubao.start(config, deliver, onError);
      return;
    }
    if (httpProvider) {
      await this.httpAsr.start(config, deliver, onError);
      return;
    }
    if (localProvider) {
      await this.local.start(config, hotwordOptions, deliver, onError);
      return;
    }
    this.generation = (this.generation + 1) % 1000000000;
    this.sessionId = VoiceRecognitionPolicy.sessionId(this.generation);
    const session: string = this.sessionId;
    if (this.engine === undefined) {
      const created: speechRecognizer.SpeechRecognitionEngine =
        await speechRecognizer.createEngine({
        language: VoiceRecognitionPolicy.language(language),
        // CoreSpeechKit's offline recognizer is the platform-specific Harmony path.
        online: 1,
        extraParams: { recognizerMode: 'short' }
      });
      // shutdown() can run while the framework is still creating the engine. Do not install a
      // late engine into a torn-down ability; it would retain the microphone service until the
      // next recognizer instance happened to replace it.
      if (session !== this.sessionId || this.resultHandler === undefined) {
        try {
          created.shutdown();
        } catch (error) {
          // The framework may have released the engine while creation was in flight.
        }
        return;
      }
      this.engine = created;
      this.engine.setListener({
        onStart: (): void => {},
        onEvent: (): void => {},
        onResult: (session: string, result: speechRecognizer.SpeechRecognitionResult): void => {
          if (session !== this.sessionId || this.resultHandler === undefined) {
            return;
          }
          // `isFinal` closes one sentence; only `isLast` closes the session, so earlier sentences are kept and prefixed.
          const whole: string = this.sentences.accept(VoiceRecognitionPolicy.result(result.result), result.isFinal);
          this.deliver(config, whole, result.isLast);
        },
        onComplete: (session: string): void => {
          if (session === this.sessionId) this.deliver(config, this.lastResultText, true);
        },
        onError: (session: string, code: number, message: string): void => {
          if (session !== this.sessionId) return;
          // CoreSpeechKit errors are terminal for the write-audio session. Stop the capturer
          // before notifying the view; otherwise the view becomes idle while this recognizer
          // continues to hold the microphone and every subsequent start is rejected as busy.
          const handler: VoiceErrorHandler | undefined = this.errorHandler;
          this.cancelSystemSession();
          this.resultHandler = undefined;
          this.errorHandler = undefined;
          if (handler !== undefined) handler(`语音识别失败（${code}）`);
        }
      });
    }
    this.sentences.reset();
    this.slicer.reset();
    if (session !== this.sessionId || this.engine === undefined || this.resultHandler === undefined) {
      return;
    }
    // Mode 0 is write-audio: the recognizer does not open the microphone itself and hears only what writeAudio hands it.
    this.engine.startListening({
      sessionId: session,
      audioInfo: {
        audioType: 'pcm', sampleRate: 16000, soundChannel: 1, sampleBit: 16
      },
      extraParams: { recognitionMode: 0, recognizerMode: 'short' }
    });
    try {
      await this.capture.start(config.capture_backend, config.capture_device, (data: ArrayBuffer): void => {
        this.writeSystemAudio(session, data);
      });
    } catch (error) {
      if (session !== this.sessionId) return;
      KeyboardLog.error(`系统语音采集启动失败：${describeError(error)}`);
      this.cancelSystemSession();
      throw new Error('语音采集启动失败');
    }
  }

  stop(): void {
    if (this.doubao.isActive()) {
      this.doubao.stop();
      return;
    }
    if (this.httpAsr.isActive()) {
      this.httpAsr.stop();
      return;
    }
    if (this.local.isActive()) {
      this.local.stop();
      return;
    }
    if (this.engine !== undefined && this.sessionId.length > 0) {
      const session: string = this.sessionId;
      void this.capture.stop().then((): void => {
        if (session !== this.sessionId || this.engine === undefined) return;
        // The last partial frame is padded with silence, since writeAudio accepts only whole frames.
        const tail: Uint8Array | null = this.slicer.flush();
        if (tail !== null) this.engine.writeAudio(session, tail);
        this.engine.finish(session);
      });
    }
  }

  cancel(): void {
    this.polishGeneration += 1;
    this.polisher.cancel();
    this.doubao.cancel();
    this.httpAsr.cancel();
    this.local.cancel();
    this.cancelSystemSession();
    this.lastResultText = '';
    this.finalDelivered = false;
  }

  shutdown(): void {
    this.polishGeneration += 1;
    this.polisher.cancel();
    this.doubao.shutdown();
    this.httpAsr.shutdown();
    this.local.shutdown();
    try {
      this.cancel();
    } catch (error) {
      // A recognizer that already completed rejects cancel; shutdown must still release it.
      this.sessionId = '';
    }
    if (this.engine !== undefined) {
      try {
        this.engine.shutdown();
      } catch (error) {
        // The service may already have released a failed engine.
      }
      this.engine = undefined;
    }
    this.resultHandler = undefined;
    this.errorHandler = undefined;
  }

  private writeSystemAudio(session: string, data: ArrayBuffer): void {
    if (session !== this.sessionId || this.engine === undefined) return;
    for (const frame of this.slicer.push(data)) {
      this.engine.writeAudio(session, frame);
    }
  }

  private cancelSystemSession(): void {
    void this.capture.stop();
    this.slicer.reset();
    this.sentences.reset();
    const session: string = this.sessionId;
    this.sessionId = '';
    if (this.engine !== undefined && session.length > 0) {
      this.engine.cancel(session);
    }
  }

  private deliver(config: VoiceInputConfiguration, text: string, final: boolean): void {
    const handler: VoiceResultHandler | undefined = this.resultHandler;
    if (handler === undefined) return;
    const normalized: string = VoiceRecognitionPolicy.result(text);
    if (!final) {
      if (normalized.length > 0) this.lastResultText = normalized;
      handler(normalized, false);
      return;
    }
    if (this.finalDelivered) return;
    this.finalDelivered = true;
    const finalText: string = normalized.length > 0 ? normalized : this.lastResultText;
    if (finalText.length === 0 || (!config.polish_enabled && !config.polish_text)) {
      handler(finalText, true);
      return;
    }
    const polishGeneration: number = ++this.polishGeneration;
    void this.polisher.polish(config, finalText).then((polished: string | null): void => {
      if (polishGeneration !== this.polishGeneration || this.resultHandler === undefined) return;
      handler(polished ?? finalText, true);
    }).catch((): void => {
      if (polishGeneration === this.polishGeneration && this.resultHandler !== undefined) {
        handler(finalText, true);
      }
    });
  }
}
