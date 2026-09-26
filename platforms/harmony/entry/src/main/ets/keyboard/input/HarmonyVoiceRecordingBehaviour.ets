/**
 * What happens around a recording, as distinct from the recording itself.
 *
 * Windows plays a short tone when recording starts and another when it ends, and can quieten other
 * applications for the duration. The shared settings page has offered all four switches on this host
 * from the beginning — its own text promises that "录音期间的提示音与静音由输入法在本机处理" — and
 * none of them did anything here.
 *
 * The tones are the same two files the Windows installer ships, played through an AVPlayer that is
 * created and released per tone: a keyboard extension is not a media application, and holding a
 * player open between recordings would keep an audio pipeline alive for a sound lasting under a
 * second. Quietening others is an audio session with a concurrency mode, which the framework applies
 * to whatever else is playing and takes back when the session is deactivated.
 *
 * Nothing here can fail a recording. A device with no media player, a session the framework refuses,
 * a missing asset — all of them leave the recording running and say so in the log.
 */
import audio from '@ohos.multimedia.audio';
import media from '@ohos.multimedia.media';
import common from '@ohos.app.ability.common';
import { KeyboardLog, describeError } from '../KeyboardLog';
import { VoiceInputConfiguration } from './VoiceInputConfiguration';
import { VoiceRecordingBehaviourPolicy } from './VoiceRecordingBehaviourPolicy';

const START_TONE: string = 'audios/start.mp3';
const END_TONE: string = 'audios/end.mp3';
// A broken decoder or an audio service that never emits a terminal state must not hold the
// recording lifecycle open forever. Releasing the player in the timeout's finally path also
// closes the raw descriptor and lets the microphone start.
const TONE_PLAYBACK_TIMEOUT_MS: number = 3000;

export class HarmonyVoiceRecordingBehaviour {
  private context: common.Context | undefined = undefined;
  private quietened: boolean = false;

  /** The extension's context, which is what resolves a rawfile into a playable descriptor. */
  use(context: common.Context): void {
    this.context = context;
  }

  /**
   * Recording is about to start: quieten others if asked, then play the opening tone.
   *
   * In that order, so the tone is not itself ducked by the session it just asked for.
   */
  async starting(voice: VoiceInputConfiguration): Promise<void> {
    await this.quieten(VoiceRecordingBehaviourPolicy.quietensOthers(voice));
    if (VoiceRecordingBehaviourPolicy.playsStartTone(voice)) {
      await this.play(START_TONE);
    }
  }

  /** Recording has ended: the closing tone, and whatever was quietened comes back. */
  async finished(voice: VoiceInputConfiguration): Promise<void> {
    if (VoiceRecordingBehaviourPolicy.playsEndTone(voice)) {
      await this.play(END_TONE);
    }
    await this.quieten(false);
  }

  /** A cancelled recording gets no closing tone; nothing was recognised to announce. */
  async cancelled(): Promise<void> {
    await this.quieten(false);
  }

  private async quieten(wanted: boolean): Promise<void> {
    if (wanted === this.quietened) {
      return;
    }
    try {
      const manager: audio.AudioSessionManager = audio.getAudioManager().getSessionManager();
      if (wanted) {
        await manager.activateAudioSession(
          { concurrencyMode: audio.AudioConcurrencyMode.CONCURRENCY_PAUSE_OTHERS });
      } else {
        await manager.deactivateAudioSession();
      }
      this.quietened = wanted;
    } catch (error) {
      // The recording is the point; the quiet is a preference. Say so and carry on.
      KeyboardLog.error(`voice audio session unavailable: ${describeError(error)}`);
      this.quietened = false;
    }
  }

  private async play(asset: string): Promise<void> {
    const context: common.Context | undefined = this.context;
    if (context === undefined) {
      return;
    }
    let player: media.AVPlayer | undefined = undefined;
    let rawFdOpen: boolean = false;
    try {
      const file = await context.resourceManager.getRawFd(asset);
      rawFdOpen = true;
      player = await media.createAVPlayer();
      const created: media.AVPlayer = player;
      created.fdSrc = { fd: file.fd, offset: file.offset, length: file.length };
      await new Promise<void>((resolve: () => void): void => {
        let finished: boolean = false;
        let timeoutHandle: number | undefined = undefined;
        const finish = (): void => {
          if (finished) {
            return;
          }
          finished = true;
          if (timeoutHandle !== undefined) {
            clearTimeout(timeoutHandle);
            timeoutHandle = undefined;
          }
          resolve();
        };
        // Resolved from whichever comes first: the tone finishing, or the player giving up. Waiting
        // on only the former would hang the caller on a device that cannot decode the asset.
        created.on('stateChange', (state: string): void => {
          if (state === 'initialized') {
            created.prepare().catch((error: Object): void => {
              KeyboardLog.error(`voice tone would not prepare: ${describeError(error)}`);
              finish();
            });
            return;
          }
          if (state === 'prepared') {
            created.play().catch((error: Object): void => {
              KeyboardLog.error(`voice tone would not play: ${describeError(error)}`);
              finish();
            });
            return;
          }
          if (state === 'completed' || state === 'error' || state === 'stopped') {
            finish();
          }
        });
        created.on('error', (error: Object): void => {
          KeyboardLog.error(`voice tone failed: ${describeError(error)}`);
          finish();
        });
        timeoutHandle = setTimeout((): void => {
          KeyboardLog.error('voice tone playback timed out');
          finish();
        }, TONE_PLAYBACK_TIMEOUT_MS);
      });
    } catch (error) {
      KeyboardLog.error(`voice tone unavailable: ${describeError(error)}`);
    } finally {
      if (rawFdOpen) {
        try {
          await context.resourceManager.closeRawFd(asset);
        } catch (error) {
          KeyboardLog.error(`voice tone raw file not closed: ${describeError(error)}`);
        }
      }
      if (player !== undefined) {
        try {
          await player.release();
        } catch (error) {
          KeyboardLog.error(`voice tone player not released: ${describeError(error)}`);
        }
      }
    }
  }
}
