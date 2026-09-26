import InputMethodExtensionAbility from '@ohos.InputMethodExtensionAbility';
import inputMethodEngine from '@ohos.inputMethodEngine';
import Want from '@ohos.app.ability.Want';
import { KeyboardSession, KeyboardPreferences } from '../keyboard/KeyboardSession';
import { CandidateAnchor, CandidateAnchorPolicy } from './CandidateAnchorPolicy';
import { KeyboardLog, describeError } from '../keyboard/KeyboardLog';
import fs from '@ohos.file.fs';
import display from '@ohos.display';
import window from '@ohos.window';
import ConfigurationConstant from '@ohos.app.ability.ConfigurationConstant';
import { KeyboardMetrics } from '../keyboard/KeyboardMetrics';
import { KeyboardFormFactor } from '../keyboard/KeyboardFormFactor';
import { HardwareKeyRouter, HardwareKey, HardwareKeyAction, HardwareKeyDecision }
  from '../keyboard/HardwareKeyRouter';
import { InputModeRouting, ModeGesture, ModeKey } from '../keyboard/InputModeRouting';
import { VoiceKey } from '../keyboard/input/VoiceHotkeyPolicy';
import { CandidateTextEdge } from '../keyboard/input/CandidateTextPolicy';
import { FloatingToolbarLayout } from '../keyboard/FloatingToolbarLayout';
import { FloatingToolbarDragPolicy } from '../keyboard/FloatingToolbarDragPolicy';
import { DesktopSurface } from '../keyboard/DesktopSurface';
import type { KeyEvent as InputKeyEvent } from '@ohos.multimodalInput.keyEvent';
import { Action as KeyAction } from '@ohos.multimodalInput.keyEvent';
import { StagedResources } from '../keyboard/StagedResources';
import { HardwareKeyDispatch } from '../keyboard/HardwareKeyDispatch';
import { PanelShortcutKey, PanelShortcutPolicy, PanelSurfaceAction }
  from '../keyboard/input/PanelShortcutPolicy';
import { ToolbarButton } from '../keyboard/FloatingToolbarLayout';
import { AttachedInputDevice, DeviceChange, HardwareKeyboardPolicy, KeyRoutingTransition }
  from '../keyboard/input/HardwareKeyboardPolicy';
import { HarmonyHardwareKeyboards } from '../keyboard/input/HarmonyHardwareKeyboards';
import { HardwareBackspaceGuard } from '../keyboard/input/BackspaceHoldPolicy';

// The badge's own geometry, from platforms/macos/src/InputModeHUDPanel.mm: a capsule that carries
// the logo and one character side by side, shown long enough to read and gone before the next word.
const HUD_WIDTH_VP: number = 100;
const HUD_HEIGHT_VP: number = 56;
const HUD_SCREEN_MARGIN_VP: number = 8;
const HUD_CARET_GAP_VP: number = 10;
const HUD_VISIBLE_MS: number = 780;
const BUNDLE_NAME: string = 'app.msime.client';
// KEYCODE_DEL, the Backspace key.
const KEYCODE_DEL: number = 2055;
const TOOLBAR_MARGIN_VP: number = 20;
// Wide enough for the key rows to keep the proportions they have on a phone, rather than stretching
// a ten-key row across a desktop.
const DESKTOP_SURFACE_WIDTH_VP: number = 520;

KeyboardLog.error('module loaded');
const engine: inputMethodEngine.InputMethodAbility = inputMethodEngine.getInputMethodAbility();

/**
 * The HarmonyOS input method host. The system starts it when an editor takes focus, so the panel is
 * created here rather than by opening a window of our own.
 *
 * The same host serves every form factor rather than being duplicated per device. What it creates
 * differs: a phone gets a soft keyboard fixed to the bottom of the screen, a 2in1 gets a candidate
 * window, because there the keys are already under the user's hands.
 */
export default class KeyboardExtensionAbility extends InputMethodExtensionAbility {
  private panel: inputMethodEngine.Panel | undefined = undefined;
  // The editor that already has focus when the panel finishes being created. The system sends
  // inputStart as soon as the ability is up, which is well before createPanel and setUiContent have
  // resolved, so an editor that arrives in that window is remembered and served once there is a panel
  // to show. Subscribing after the awaits instead would simply drop it, and the keyboard would sit
  // there visible but inert until the user focused something else.
  private pending: inputMethodEngine.InputClient | undefined = undefined;
  // Where the caret last was, in px, so the candidate window can be put under it. Kept here because
  // the panel is here: the view has no handle on the window it is drawn into.
  private caret: [number, number, number] = [0, 0, 0];
  private candidateAnchor: CandidateAnchor | undefined = undefined;
  private density: number = 1;
  // The badge that says which mode a switch landed in. Its own window, because it has to be able to
  // appear when the candidate window is not up: a mode is switched between words, not during one.
  private hud: inputMethodEngine.Panel | undefined = undefined;
  private hudTimer: number = -1;
  private readonly modeKeys: InputModeRouting = new InputModeRouting();
  private readonly backspaceHold: HardwareBackspaceGuard = new HardwareBackspaceGuard();
  // The bar that says what mode the keyboard is in. Another window of its own, because it outlives
  // every composition and every editor: it is on screen whenever the keyboard is running at all.
  private toolbar: inputMethodEngine.Panel | undefined = undefined;
  // Preferences can be reloaded again while a STATUS_BAR panel is still being created. The
  // generation fences those awaits so an obsolete request cannot leak a panel or tear down the
  // panel belonging to a newer request.
  private statusPanelGeneration: number = 0;
  private toolbarPosition: [number, number] = [0, 0];
  // Set while the toolbar is hidden because the focused editor's window fills the screen.
  private toolbarHiddenForFullscreen: boolean = false;
  private toolbarDragOrigin: [number, number] = [0, 0];
  private screenWidth: number = 0;
  private screenHeight: number = 0;
  private candidateCount: number = 0;
  private candidateWidthVp: number = KeyboardMetrics.CANDIDATE_WINDOW_WIDTH_VP;
  // The attached keyboards, and whether their keys are currently being routed. Tracked rather than
  // re-derived because a second on('keyEvent') is not idempotent: it would deliver every keystroke
  // twice, and the framework de-duplicates nothing.
  private attachedKeyboards: AttachedInputDevice[] = [];
  private routingKeys: boolean = false;
  /** Set once the ability has torn down, so a late device enumeration cannot route keys again. */
  private tornDown: boolean = false;

  /** The editor moved its caret; the candidate window goes with it if it is up. */
  private readonly caretMoved: (x: number, y: number, height: number) => void =
    (x: number, y: number, height: number): void => {
      this.caret = [x, y, height];
      KeyboardSession.shared.invalidateReplyContext();
      // When follow-cursor is disabled, the first composition position is the anchor for the
      // whole candidate window lifetime. Keep recording the latest caret while idle so the next
      // composition still starts beside the newly focused text.
      if (KeyboardSession.shared.composing()
        && KeyboardSession.shared.preferences().candidateFollowCursor) {
        this.placeCandidateWindow();
      }
    };

  /** The editor changed its selection, invalidating everything derived from the old caret. */
  private readonly selectionChanged: (oldBegin: number, oldEnd: number,
                                      newBegin: number, newEnd: number) => void =
    (_oldBegin: number, _oldEnd: number, _newBegin: number, _newEnd: number): void => {
      KeyboardSession.shared.editorSelectionChanged();
    };

  /** The editor changed its document, either as our edit echo or independently of the keyboard. */
  private readonly textChanged: (value: string) => void = (_value: string): void => {
    KeyboardSession.shared.editorTextChanged();
  };

  /**
   * As wide as the buttons the user left on, scaled by what they asked for.
   *
   * The width is computed from the same layout the page draws with, so a hidden button narrows the
   * bar rather than leaving a gap where it was.
   */
  private async resizeToolbar(density: number,
                              panel: inputMethodEngine.Panel | undefined = this.toolbar): Promise<void> {
    if (panel === undefined) {
      return;
    }
    const session: KeyboardSession = KeyboardSession.shared;
    const count: number = FloatingToolbarLayout.buttons(session.toolbarComponents()).length;
    const scale: number = session.toolbarScale();
    await panel.resize(
      Math.round(FloatingToolbarLayout.widthVp(count) * scale * density),
      Math.round(FloatingToolbarLayout.heightVp() * scale * density));
  }

  /**
   * The toolbar or the mode badge, whichever the preferences ask for, in the one STATUS_BAR panel.
   *
   * Measured on a 2in1: the framework grants one STATUS_BAR panel, not two. The toolbar takes it when there is one, and the toolbar is the better readout anyway — it says what mode the keyboard is in all the time rather than for three quarters of a second after it changed. The badge is what a desktop falls back to when the toolbar is turned off, which is exactly when nothing else on screen would say the mode had changed at all. The badge is also the one piece of feedback the user can turn off from the shared settings, and a panel that would never be shown is not worth the one STATUS_BAR grant to hold open.
   *
   * Run at creation and again whenever the settings page changes the preferences, so turning the toolbar on or off takes effect without restarting the input method, as the Windows host's config sync does. Whatever is to be closed is closed before anything is opened, since the grant has to be free first.
   */
  private async reconcileStatusPanels(): Promise<void> {
    const generation: number = ++this.statusPanelGeneration;
    const session: KeyboardSession = KeyboardSession.shared;
    const wantToolbar: boolean = session.toolbarEnabled();
    const wantBadge: boolean = !wantToolbar && session.modeBadgeEnabled();
    if (!wantToolbar && this.toolbar !== undefined) {
      this.closeToolbar();
    }
    if (!wantBadge && this.hud !== undefined) {
      this.closeModeBadge();
    }
    if (wantToolbar && this.toolbar === undefined) {
      await this.openToolbar(generation);
      if (generation !== this.statusPanelGeneration) return;
    } else if (this.toolbar !== undefined) {
      await this.resizeToolbar(this.density);
      if (generation !== this.statusPanelGeneration) return;
      session.refreshToolbar();
    }
    if (wantBadge && this.hud === undefined) {
      await this.openModeBadge(generation);
    }
  }

  private async openToolbar(generation: number): Promise<void> {
    let toolbar: inputMethodEngine.Panel | undefined = undefined;
    try {
      toolbar = await engine.createPanel(this.context, {
        type: inputMethodEngine.PanelType.STATUS_BAR
      });
      if (this.tornDown || generation !== this.statusPanelGeneration) {
        await engine.destroyPanel(toolbar);
        return;
      }
      await toolbar.setUiContent('keyboard/FloatingToolbar');
      if (this.tornDown || generation !== this.statusPanelGeneration) {
        await engine.destroyPanel(toolbar);
        return;
      }
      this.toolbar = toolbar;
      KeyboardSession.shared.onToolbarDragStart = (): void => {
        this.beginToolbarDrag();
      };
      KeyboardSession.shared.onToolbarDrag = (offsetX: number, offsetY: number): void => {
        this.dragToolbar(offsetX, offsetY);
      };
      await this.resizeToolbar(this.density, toolbar);
      if (this.tornDown || generation !== this.statusPanelGeneration) {
        // A newer reconciliation may have adopted this panel. Leave it to that call (or
        // teardown) rather than destroying the panel it now owns.
        if (this.toolbar !== toolbar) {
          await engine.destroyPanel(toolbar);
        }
        return;
      }
      KeyboardSession.shared.refreshToolbar();
      // Lower right by default, clear of the dock, where a status bar is looked for and where it is least likely to sit on top of what is being written. macOS picks the same corner.
      const width: number = FloatingToolbarLayout.widthVp(
        FloatingToolbarLayout.buttons(KeyboardSession.shared.toolbarComponents()).length)
        * KeyboardSession.shared.toolbarScale() * this.density;
      const height: number = FloatingToolbarLayout.heightVp()
        * KeyboardSession.shared.toolbarScale() * this.density;
      const margin: number = TOOLBAR_MARGIN_VP * this.density;
      this.toolbarPosition = [Math.round(this.screenWidth - width - margin),
        Math.round(this.screenHeight - height - margin * 4)];
      await toolbar.moveTo(this.toolbarPosition[0], this.toolbarPosition[1]);
      if (this.tornDown || generation !== this.statusPanelGeneration) {
        if (this.toolbar !== toolbar) {
          await engine.destroyPanel(toolbar);
        }
        return;
      }
      await toolbar.show();
    } catch (error) {
      KeyboardLog.error(`toolbar unavailable: ${describeError(error)}`);
      if (toolbar !== undefined
        && (generation === this.statusPanelGeneration || this.toolbar !== toolbar)) {
        if (this.toolbar === toolbar) {
          this.toolbar = undefined;
          KeyboardSession.shared.onToolbarDragStart = undefined;
          KeyboardSession.shared.onToolbarDrag = undefined;
        }
        engine.destroyPanel(toolbar);
      }
    }
  }

  private closeToolbar(): void {
    if (this.toolbar === undefined) {
      return;
    }
    KeyboardSession.shared.onToolbarDragStart = undefined;
    KeyboardSession.shared.onToolbarDrag = undefined;
    engine.destroyPanel(this.toolbar);
    this.toolbar = undefined;
    this.toolbarHiddenForFullscreen = false;
  }

  private async openModeBadge(generation: number): Promise<void> {
    let hud: inputMethodEngine.Panel | undefined = undefined;
    try {
      hud = await engine.createPanel(this.context, {
        type: inputMethodEngine.PanelType.STATUS_BAR
      });
      if (this.tornDown || generation !== this.statusPanelGeneration) {
        await engine.destroyPanel(hud);
        return;
      }
      await hud.setUiContent('keyboard/InputModeHud');
      if (this.tornDown || generation !== this.statusPanelGeneration) {
        await engine.destroyPanel(hud);
        return;
      }
      this.hud = hud;
      await hud.resize(Math.round(HUD_WIDTH_VP * this.density),
        Math.round(HUD_HEIGHT_VP * this.density));
      if (this.tornDown || generation !== this.statusPanelGeneration) {
        if (this.hud !== hud) {
          await engine.destroyPanel(hud);
        }
      }
    } catch (error) {
      // Worth saying but not worth failing over: the keyboard types perfectly well without a badge, and refusing to start because a piece of feedback would not build helps nobody.
      KeyboardLog.error(`mode badge unavailable: ${describeError(error)}`);
      if (hud !== undefined
        && (generation === this.statusPanelGeneration || this.hud !== hud)) {
        if (this.hud === hud) this.hud = undefined;
        engine.destroyPanel(hud);
      }
    }
  }

  private closeModeBadge(): void {
    if (this.hudTimer >= 0) {
      clearTimeout(this.hudTimer);
      this.hudTimer = -1;
    }
    if (this.hud !== undefined) {
      engine.destroyPanel(this.hud);
      this.hud = undefined;
    }
  }

  /** The settings page saved new preferences and the session has been rebuilt from them. */
  private preferencesReloaded(): void {
    this.modeKeys.use(KeyboardSession.shared.modeBindings());
    if (!KeyboardFormFactor.isDesktop() || this.panel === undefined) {
      return;
    }
    this.reconcileStatusPanels().catch((error: Object): void => {
      KeyboardLog.error(`status panels not reconciled: ${describeError(error)}`);
    });
  }

  private beginToolbarDrag(): void {
    this.toolbarDragOrigin = [this.toolbarPosition[0], this.toolbarPosition[1]];
  }

  private dragToolbar(offsetX: number, offsetY: number): void {
    if (this.toolbar === undefined) {
      return;
    }
    const session: KeyboardSession = KeyboardSession.shared;
    const count: number = FloatingToolbarLayout.buttons(session.toolbarComponents()).length;
    const scale: number = session.toolbarScale();
    const width: number = FloatingToolbarLayout.widthVp(count) * scale * this.density;
    const height: number = FloatingToolbarLayout.heightVp() * scale * this.density;
    const position: [number, number] = FloatingToolbarDragPolicy.position(
      this.toolbarDragOrigin, [offsetX, offsetY], this.density, this.screenWidth,
      this.screenHeight, width, height);
    this.toolbarPosition = position;
    this.toolbar.moveTo(position[0], position[1]).catch((error: Object): void => {
      KeyboardLog.error(`toolbar would not move: ${describeError(error)}`);
    });
  }

  /**
   * Open, close or swap the surface the candidate window is carrying.
   *
   * The same window, because there is one: a keyboard and a grid of emoji are both too big to sit
   * where the candidates do, so it is resized and moved rather than duplicated. Closing puts it back
   * to the size a candidate list needs, so the next composition does not open a keyboard-sized hole.
   */
  private showDesktopSurface(surface: DesktopSurface): void {
    const panel: inputMethodEngine.Panel | undefined = this.panel;
    if (panel === undefined) {
      return;
    }
    if (surface === DesktopSurface.NONE) {
      this.resizeCandidateWindow(surface).then((): void => {
        if (!KeyboardSession.shared.composing()) {
          panel.hide();
        }
      }).catch((error: Object): void => {
        KeyboardLog.error(`surface would not close: ${describeError(error)}`);
      });
      return;
    }
    // Shown before it is sized, not after: a panel that is not on screen takes a resize without
    // complaint and then appears at whatever size it had before, which on screen is a candidate
    // strip where a keyboard was asked for.
    panel.show().then((): Promise<void> => this.resizeCandidateWindow(surface)).then((): void => {
      this.centreCandidateWindow(surface);
    }).catch((error: Object): void => {
      KeyboardLog.error(`surface would not open: ${describeError(error)}`);
    });
  }

  /** A candidate list is one strip; the other two are as tall as the keyboard they stand in for. */
  private async resizeCandidateWindow(surface: DesktopSurface): Promise<void> {
    if (this.panel === undefined) {
      return;
    }
    const settings: KeyboardPreferences = KeyboardSession.shared.preferences();
    const tall: boolean = surface !== DesktopSurface.NONE;
    const heightVp: number = tall
      ? KeyboardMetrics.totalHeightVp(settings.rowSpacingTenths, settings.heightAdjustment,
        KeyboardSession.shared.glossRows(), settings.candidateFontSize,
        settings.candidatePreeditFontSize, true)
      : KeyboardMetrics.candidateHeightVp(settings.candidateLayout, this.candidateCount,
        settings.candidateShowPreedit, settings.candidateDecorationTopVp,
        KeyboardSession.shared.glossRows(), settings.candidateFontSize,
        settings.candidatePreeditFontSize);
    const widthVp: number = tall ? DESKTOP_SURFACE_WIDTH_VP : this.candidateWidthVp;
    await this.panel.resize(Math.round(widthVp * this.density),
      Math.round(heightVp * this.density));
  }

  /**
   * A surface that was asked for goes where it can be seen rather than where the caret is.
   *
   * The caret is the right place for candidates, which belong to the word being typed. A keyboard or
   * a grid of emoji belongs to the session, so it sits at the bottom of the screen the way one does
   * on a phone, out of the way of the text above it.
   */
  private centreCandidateWindow(surface: DesktopSurface): void {
    if (this.panel === undefined || this.screenWidth === 0) {
      return;
    }
    const settings: KeyboardPreferences = KeyboardSession.shared.preferences();
    const height: number = KeyboardMetrics.totalHeightVp(
      settings.rowSpacingTenths, settings.heightAdjustment,
      KeyboardSession.shared.glossRows(), settings.candidateFontSize,
      settings.candidatePreeditFontSize, true) * this.density;
    const width: number = DESKTOP_SURFACE_WIDTH_VP * this.density;
    this.panel.moveTo(Math.round((this.screenWidth - width) / 2),
      Math.round(this.screenHeight - height - TOOLBAR_MARGIN_VP * this.density))
      .catch((error: Object): void => {
        KeyboardLog.error(`surface would not move: ${describeError(error)}`);
      });
  }

  /** The gear opens the application, which is where everything that is not a keystroke lives. */
  private openSettings(): void {
    // Named rather than read off the context: an extension context carries no ability info, and the
    // bundle is this one by construction — the panel and the settings window are the same app.
    this.context.startAbility({
      bundleName: BUNDLE_NAME, abilityName: 'EntryAbility'
    }).catch((error: Object): void => {
      KeyboardLog.error(`settings would not open: ${describeError(error)}`);
    });
  }

  /**
   * Flip the composing language and say so.
   *
   * The badge is the only feedback there is on a machine with no keyboard on screen to change shape:
   * nothing else on a desktop tells the user which mode the next letter will be in.
   */
  private switchInputMode(): void {
    this.modeKeys.disarm();
    const english: boolean = KeyboardSession.shared.toggleLanguage();
    KeyboardSession.shared.refreshToolbar();
    this.showModeBadge(english);
  }

  /**
   * Show the badge, then take it away again.
   *
   * Long enough to be read at a glance, short enough that it is gone before the next word is typed.
   * A switch arriving while one is still up restarts the wait rather than stacking a second timer.
   */
  private showModeBadge(english: boolean): void {
    if (this.hud === undefined) {
      return;
    }
    const panel: inputMethodEngine.Panel = this.hud;
    KeyboardSession.shared.setModeBadge(english);
    this.placeModeBadge();
    panel.show().catch((error: Object): void => {
      KeyboardLog.error(`mode badge would not show: ${describeError(error)}`);
    });
    if (this.hudTimer >= 0) {
      clearTimeout(this.hudTimer);
    }
    this.hudTimer = setTimeout((): void => {
      this.hudTimer = -1;
      panel.hide().catch((error: Object): void => {
        KeyboardLog.error(`mode badge would not hide: ${describeError(error)}`);
      });
    }, HUD_VISIBLE_MS);
  }

  /** Under the caret, clamped inside the screen; centred low when the editor reports no caret. */
  private placeModeBadge(): void {
    if (this.hud === undefined) {
      return;
    }
    const width: number = HUD_WIDTH_VP * this.density;
    const height: number = HUD_HEIGHT_VP * this.density;
    const margin: number = HUD_SCREEN_MARGIN_VP * this.density;
    let x: number = this.caret[2] > 0
      ? this.caret[0] + this.caret[2] / 2 - width / 2
      : this.screenWidth / 2 - width / 2;
    let y: number = this.caret[2] > 0
      ? this.caret[1] + this.caret[2] + HUD_CARET_GAP_VP * this.density
      : this.screenHeight * 2 / 3;
    x = Math.min(Math.max(margin, x), Math.max(margin, this.screenWidth - margin - width));
    y = Math.min(Math.max(margin, y), Math.max(margin, this.screenHeight - margin - height));
    this.hud.moveTo(Math.round(x), Math.round(y)).catch((error: Object): void => {
      KeyboardLog.error(`mode badge would not move: ${describeError(error)}`);
    });
  }

  /**
   * Put the candidate window just below the caret, kept inside the screen.
   *
   * Below by default, because that is where the eye goes next; above when there is no room beneath,
   * so the window never hangs off the bottom edge. The same clamp on the other axis keeps a caret
   * near the right edge from pushing the window off the side.
   */
  private placeCandidateWindow(): void {
    if (this.panel === undefined) {
      return;
    }
    const width: number = this.candidateWidthVp * this.density;
    const settings: KeyboardPreferences = KeyboardSession.shared.preferences();
    const height: number = KeyboardMetrics.candidateHeightVp(settings.candidateLayout,
      this.candidateCount, settings.candidateShowPreedit,
      settings.candidateDecorationTopVp, KeyboardSession.shared.glossRows(),
      settings.candidateFontSize, settings.candidatePreeditFontSize) * this.density;
    const gap: number = KeyboardMetrics.CANDIDATE_WINDOW_GAP_VP * this.density;
    const anchor: CandidateAnchor = CandidateAnchorPolicy.position(settings.candidateFollowCursor,
      this.candidateAnchor, this.caret);
    // A caret of no height is one that was never reported: a web view mid-layout, a client that
    // answers with zeroes, or simply an editor that has not sent one yet. Believing it puts the
    // window in the top-left corner of the screen, which is where this went. Low and centred is
    // where a desktop input method puts a candidate list it cannot place precisely.
    if (anchor[2] <= 0) {
      this.panel.moveTo(Math.round(Math.max(0, (this.screenWidth - width) / 2)),
        Math.round(Math.max(0, this.screenHeight * 2 / 3)))
        .catch((error: Object): void => {
          KeyboardLog.error(`candidate window would not move: ${describeError(error)}`);
        });
      return;
    }
    let x: number = anchor[0];
    if (this.screenWidth > 0 && x + width > this.screenWidth) {
      x = this.screenWidth - width;
    }
    let y: number = anchor[1] + anchor[2] + gap;
    if (this.screenHeight > 0 && y + height > this.screenHeight) {
      y = anchor[1] - height - gap;
    }
    this.panel.moveTo(Math.round(Math.max(0, x)), Math.round(Math.max(0, y)))
      .catch((error: Object): void => {
        KeyboardLog.error(`candidate window would not move: ${describeError(error)}`);
      });
  }

  // off('inputStop') requires the same callback reference that on() received, unlike off('inputStart')
  // where it is optional. Holding both as fields keeps unsubscribing honest.
  private readonly started: (kbController: inputMethodEngine.KeyboardController,
                             inputClient: inputMethodEngine.InputClient) => void =
    (kbController: inputMethodEngine.KeyboardController,
     inputClient: inputMethodEngine.InputClient): void => {
      if (this.panel === undefined) {
        this.pending = inputClient;
        return;
      }
      this.pending = undefined;
      // A caret belongs to the editor that reported it. Carrying one across a focus change puts the
      // next composition beside the last editor's text, which is worse than not knowing at all.
      this.caret = [0, 0, 0];
      KeyboardSession.shared.attach(inputClient);
      // A keyboard belongs on screen the moment an editor takes focus; a candidate window has
      // nothing to offer until something is being spelled, and waits for onComposition to say so.
      if (!KeyboardFormFactor.isDesktop()) {
        this.panel.show();
      } else {
        this.followCallingWindow(inputClient);
      }
    };

  /**
   * Keep the floating toolbar off an application that fills the screen, and bring it back when the focus is in a window that does not.
   *
   * The Windows host hides its toolbar while the foreground window is fullscreen (`OnWinEvent` in `window_hook.cpp`), so a game or a video is not covered by a bar nobody is using. HarmonyOS tells the keyboard the calling window's status, and only when asked, so it is asked each time an editor takes focus; a window that goes fullscreen while its editor keeps the focus is noticed at the next focus.
   */
  private followCallingWindow(inputClient: inputMethodEngine.InputClient): void {
    const toolbar: inputMethodEngine.Panel | undefined = this.toolbar;
    if (toolbar === undefined) {
      return;
    }
    inputClient.getCallingWindowInfo().then((info: inputMethodEngine.WindowInfo): Promise<void> | undefined => {
      const fullscreen: boolean = info.status === window.WindowStatusType.FULL_SCREEN;
      if (this.toolbar !== toolbar || fullscreen === this.toolbarHiddenForFullscreen) {
        return undefined;
      }
      this.toolbarHiddenForFullscreen = fullscreen;
      return fullscreen ? toolbar.hide() : toolbar.show();
    }).catch((error: Object): void => {
      KeyboardLog.error(`calling window unavailable: ${describeError(error)}`);
    });
  }

  private readonly stopped: () => void = (): void => {
    // An editor that stops while the panel is still being built must not be attached once it is.
    this.pending = undefined;
    this.backspaceHold.reset();
    KeyboardSession.shared.detach();
    if (this.panel !== undefined) {
      this.panel.hide();
    }
  };

  /**
   * A key from a real keyboard, on a machine that has one.
   *
   * Returning false hands the key to the application untouched, which is what happens to everything
   * the router does not claim. Only key-down is acted on: a key that was claimed going down must be
   * claimed coming up too, or the application sees half of it.
   */
  /** Key codes whose key-down went back to the application, so their key-up must follow it. */
  private readonly releasedKeys: Set<number> = new Set<number>();
  /** Key codes whose key-down the emoji panel took, so their key-up is taken too. */
  private readonly panelKeys: Set<number> = new Set<number>();

  private readonly typed: (event: InputKeyEvent) => boolean = (event: InputKeyEvent): boolean => {
    const key: HardwareKey = HardwareKeyRouter.normalizeLetterCase({
      keyCode: event.key.code, unicodeChar: event.unicodeChar, ctrlKey: event.ctrlKey,
      altKey: event.altKey, logoKey: event.logoKey, shiftKey: event.shiftKey
    }, event.capsLock);
    const session: KeyboardSession = KeyboardSession.shared;
    session.setCapsLock(event.capsLock);
    const mode: ModeKey = {
      keyCode: event.key.code, down: event.action === KeyAction.DOWN, shiftKey: event.shiftKey,
      ctrlKey: event.ctrlKey, altKey: event.altKey, logoKey: event.logoKey,
      timestamp: Date.now()
    };
    // The voice shortcuts come first and apply whether or not anything is being composed: Windows
    // records into whatever has focus, and a hold key that only worked mid-composition would be a
    // shortcut nobody could start. Escape and Space are claimed only while recording, so they go on
    // meaning what they mean to a composition the rest of the time.
    const voiceKey: VoiceKey = {
      keyCode: event.key.code, down: event.action === KeyAction.DOWN, ctrlKey: event.ctrlKey,
      altKey: event.altKey, shiftKey: event.shiftKey, logoKey: event.logoKey
    };
    if (session.routeVoiceHotkey(voiceKey)) {
      return true;
    }
    // The panel chord, like the voice ones, applies whether or not anything is being composed: the
    // on-screen keyboard exists for the character the physical keyboard has no key for, and needing
    // to be mid-composition to ask for it would put it out of reach exactly when it is wanted.
    const panelKey: PanelShortcutKey = {
      keyCode: event.key.code, down: event.action === KeyAction.DOWN, ctrlKey: event.ctrlKey,
      shiftKey: event.shiftKey, altKey: event.altKey, logoKey: event.logoKey
    };
    if (PanelShortcutPolicy.claims(panelKey, KeyboardFormFactor.isDesktop())) {
      const surface: PanelSurfaceAction =
        PanelShortcutPolicy.action(PanelShortcutPolicy.shortcut(panelKey,
          KeyboardFormFactor.isDesktop()));
      if (surface !== PanelSurfaceAction.NONE) {
        // Through the same entry the toolbar button uses, not the window helper underneath it. That
        // helper shows and sizes a panel but never records which surface is open, so the view went
        // on drawing candidates and the window was sized around them: a thin empty bar where a
        // keyboard had been asked for. Asking for the open surface still closes it, because that is
        // what this entry does.
        session.runToolbarButton(surface as number as ToolbarButton);
      }
      // The release is claimed too, or a bare K lands in the editor after the panel has opened.
      return true;
    }
    // Switching mode is the one thing worth claiming in an editor this keyboard will not compose
    // for: the mode outlives the field, and a password box is exactly where one notices being in the
    // wrong one. Checked before anything else, so a chord meant for the mode never reaches the
    // Engine as the letter it is built on.
    const gesture: ModeGesture = this.modeKeys.accept(mode);
    if (gesture === ModeGesture.SWITCH_LANGUAGE) {
      this.switchInputMode();
      return true;
    }
    if (gesture === ModeGesture.TOGGLE_CHARACTER_SET) {
      // Windows only switches while composing Chinese, but claims the chord either way (`HandleImeKey`, `IsCharacterSetInputModeToggle`).
      if (!session.isEnglish()) {
        session.toggleCharacterSet();
        KeyboardSession.shared.refreshToolbar();
      }
      return true;
    }
    if (gesture === ModeGesture.ENGLISH_CANDIDATES) {
      this.modeKeys.disarm();
      this.showModeBadge(session.toggleEnglishCandidates());
      KeyboardSession.shared.refreshToolbar();
      return true;
    }
    if (gesture === ModeGesture.TOGGLE_WIDTH) {
      session.toggleCharacterWidth();
      return true;
    }
    if (gesture === ModeGesture.TOGGLE_PUNCTUATION) {
      session.togglePunctuation();
      KeyboardSession.shared.refreshToolbar();
      return true;
    }
    // The 2in1 emoji panel takes navigation, activation and search keys the way the focused Windows panel does (`EmojiPanel::OnKeyDown`); its key-up follows, so the editor never sees half of one.
    if (event.action !== KeyAction.DOWN && this.panelKeys.delete(event.key.code)) {
      return true;
    }
    if (event.action === KeyAction.DOWN && session.routePanelKey({
      keyCode: event.key.code, unicodeChar: event.unicodeChar, ctrlKey: event.ctrlKey,
      altKey: event.altKey, logoKey: event.logoKey
    })) {
      this.panelKeys.add(event.key.code);
      return true;
    }
    // A space with nothing being composed is the editor's, with one exception: it is also the key
    // that rewrites a just-committed Chinese mark as ASCII, and that arming only exists after a
    // commit — which is to say, only when nothing is being composed.
    if (event.action === KeyAction.DOWN && session.convertPunctuationSpace(event.unicodeChar)) {
      return true;
    }
    // The other half of a letter the Engine declined going down: the application saw that key-down, so it gets the key-up too.
    if (event.action !== KeyAction.DOWN && this.releasedKeys.delete(event.key.code)) {
      return false;
    }
    // A Backspace held from inside a composition stops where the composition did; see HardwareBackspaceGuard.
    if (event.key.code === KEYCODE_DEL) {
      const held: boolean = event.action === KeyAction.DOWN
        ? this.backspaceHold.down(session.composing()) : this.backspaceHold.up(session.composing());
      if (held) {
        return true;
      }
    } else if (event.action === KeyAction.DOWN) {
      this.backspaceHold.reset();
    }
    if (!session.composes()) {
      return false;
    }
    const decision: HardwareKeyDecision =
      HardwareKeyRouter.route(key, session.composing(), !session.isEnglish() || session.isEnglishCandidates(),
        KeyboardSession.shared.preferences().numberRowSelection,
      KeyboardSession.shared.preferences().navigation,
        session.hasHighlightedTranslation(), session.isJapanese(),
        session.wordCharacterBinding(), session.hasHighlightedCandidate(), session.hardwareSpelling(),
        session.isFullWidth(), session.punctuationLockedChinese());
    if (decision.action === HardwareKeyAction.RELEASE) {
      // What the application types is still the user's typing, and Windows counts it (`stats_passthrough.h`).
      if (event.action === KeyAction.DOWN) {
        session.recordPassthrough(event.unicodeChar, event.ctrlKey, event.altKey, event.logoKey);
      }
      return false;
    }
    if (event.action !== KeyAction.DOWN) {
      return true;
    }
    if (!HardwareKeyDispatch.apply(decision, key.shiftKey, session)) {
      // Declined rather than swallowed, the way TSF treats a key the Windows host did not eat: a capital letter with nothing composed is the application's to type.
      this.releasedKeys.add(event.key.code);
      session.recordPassthrough(event.unicodeChar, event.ctrlKey, event.altKey, event.logoKey);
      return false;
    }
    return true;
  };

  /**
   * Routes physical keys for as long as there are physical keys to route.
   *
   * The hot-plug listener is registered before the first enumeration so that a keyboard attached
   * during startup is not missed in the gap between the two, and the enumeration is applied on top
   * of whatever the listener has already recorded rather than replacing it.
   */
  private routeKeysWhenAttached(): void {
    if (KeyboardFormFactor.isDesktop()) {
      this.applyKeyRouting();
      return;
    }
    HarmonyHardwareKeyboards.watch((change: DeviceChange): void => {
      this.attachedKeyboards = HardwareKeyboardPolicy.applyChange(this.attachedKeyboards, change);
      this.applyKeyRouting();
    });
    HarmonyHardwareKeyboards.list((devices: AttachedInputDevice[]): void => {
      for (const device of devices) {
        this.attachedKeyboards = HardwareKeyboardPolicy.applyChange(this.attachedKeyboards,
          { type: 'add', deviceId: device.deviceId, keyboardType: device.keyboardType });
      }
      this.applyKeyRouting();
    });
  }

  /** Brings the `keyEvent` subscription in line with what is attached, and only when it differs. */
  private applyKeyRouting(): void {
    if (this.tornDown) {
      return;
    }
    const route: boolean =
      HardwareKeyboardPolicy.routes(KeyboardFormFactor.isDesktop(), this.attachedKeyboards);
    const transition: KeyRoutingTransition | null =
      HardwareKeyboardPolicy.transition(this.routingKeys, route);
    if (transition === null) {
      return;
    }
    try {
      if (transition === KeyRoutingTransition.SUBSCRIBE) {
        inputMethodEngine.getKeyboardDelegate().on('keyEvent', this.typed);
        this.routingKeys = true;
        KeyboardLog.info(`routing hardware keys: ${this.attachedKeyboards.length} device(s)`);
        return;
      }
      // off('keyEvent') wants the same callback reference on() was given, which is why `typed` is a
      // property and not a method.
      inputMethodEngine.getKeyboardDelegate().off('keyEvent', this.typed);
      this.routingKeys = false;
      KeyboardLog.info('hardware keys released: no keyboard attached');
    } catch (error) {
      KeyboardLog.error(`key routing ${transition} refused: ${describeError(error)}`);
    }
  }

  /**
   * Copies the packaged dictionary into the sandbox once, and only once.
   *
   * stage-resources.sh puts the pinned release under resfile/engine, which OpenHarmony extracts to
   * resourceDir at install time, and reading those files with @ohos.file.fs works. SQLite does not:
   * opening a database on that read-only bundle mount fails even for a read-only connection, because
   * the mount does not carry the advisory locks SQLite takes, and the Engine reports it as "Unable to
   * copy runtime dictionary". So the databases are copied to filesDir, where SQLite is on ordinary
   * storage, and the Engine derives its generation from there.
   *
   * The marker sits beside the directory rather than inside it: prepare_host verifies the resource
   * directory against the pinned lock and rejects any file the lock does not name.
   */
  async onCreate(want: Want): Promise<void> {
    const files: string = this.context.filesDir;
    const resources: string = `${files}/engine`;
    const state: string = `${files}/state`;
    if (!fs.accessSync(state)) {
      fs.mkdirSync(state, true);
    }
    if (!StagedResources.stage(`${this.context.resourceDir}/engine`, resources,
        `${files}/engine.ready`)) {
      return;
    }
    // Optional non-English candidate glosses, staged beside the resources where the Engine looks for them, on the same ordinary storage SQLite needs. A package built without them removes any an earlier one left; either way the keyboard still starts.
    const glosses: string = `${this.context.resourceDir}/offline-glosses`;
    if (fs.accessSync(glosses)) {
      StagedResources.stage(glosses, `${files}/offline-glosses`, `${files}/offline-glosses.ready`);
    } else {
      StagedResources.unstage(`${files}/offline-glosses`, `${files}/offline-glosses.ready`);
    }
    // The extension context knows the system colour mode; the view has no context of its own to ask.
    const systemDark: boolean =
      this.context.config.colorMode === ConfigurationConstant.ColorMode.COLOR_MODE_DARK;
    // The packaged recording tones are read through this context, which the session has none of.
    KeyboardSession.shared.useVoiceContext(this.context);
    const started: boolean = KeyboardSession.shared.start(resources, state, systemDark);
    if (!started) {
      // Without a session there is nothing to show. Leaving the panel uncreated keeps the framework
      // from presenting an inert keyboard.
      KeyboardLog.error('onCreate: no session, panel not created');
      return;
    }
    engine.on('inputStart', this.started);
    engine.on('inputStop', this.stopped);
    inputMethodEngine.getKeyboardDelegate().on('selectionChange', this.selectionChanged);
    inputMethodEngine.getKeyboardDelegate().on('textChange', this.textChanged);
    // Which gestures are bound is the document's business, so the keyboard and the settings page
    // cannot disagree about what a key does. Bound before any key can arrive, on every device: a
    // keyboard may be plugged into this one later.
    this.modeKeys.use(KeyboardSession.shared.modeBindings());
    // Every device, not only the one with a toolbar: a phone reaches the settings application from its tools panel.
    KeyboardSession.shared.onOpenSettings = (): void => {
      this.openSettings();
    };
    KeyboardSession.shared.onPreferencesReloaded = (): void => {
      this.preferencesReloaded();
    };
    if (KeyboardFormFactor.isDesktop()) {
      // A candidate window belongs beside the text it is offering candidates for, not at a fixed
      // place on the screen, so it follows the caret the editor reports. Only a candidate window has
      // anywhere to go: a fixed soft keyboard sits at the bottom wherever the caret is.
      inputMethodEngine.getKeyboardDelegate().on('cursorContextChange', this.caretMoved);
    }
    // Where the keys are the machine's, routing them is the only way anything reaches the Engine at
    // all: nothing is ever tapped on a panel that draws no keys. Where they are not, an attached
    // keyboard still has to compose — unrouted, the framework hands the key to the editor and a
    // phone with a keyboard types raw letters — so the enumeration decides, and keeps deciding as
    // keyboards come and go.
    this.routeKeysWhenAttached();
    // A machine with its own keys gets a candidate window rather than a keyboard. The framework does
    // not show or hide a FLAG_CANDIDATE panel for you, which costs nothing here: this host has always
    // driven show and hide itself from inputStart and inputStop rather than leaving it to the
    // framework. The flag is chosen at creation instead of switched afterwards with changeFlag, which
    // requires hiding the panel first and would only be worth that if a device could change what it
    // is while running.
    const desktop: boolean = KeyboardFormFactor.isDesktop();
    const info: inputMethodEngine.PanelInfo = {
      type: inputMethodEngine.PanelType.SOFT_KEYBOARD,
      flag: desktop ? inputMethodEngine.PanelFlag.FLAG_CANDIDATE
        : inputMethodEngine.PanelFlag.FLG_FIXED
    };
    try {
      const panel: inputMethodEngine.Panel = await engine.createPanel(this.context, info);
      // onDestroy can arrive while the framework is still creating or populating the panel. Do
      // not attach a late panel after teardown; otherwise the ability leaks a window with no host
      // callbacks left to hide or destroy it.
      if (this.tornDown) {
        await engine.destroyPanel(panel);
        return;
      }
      this.panel = panel;
      await panel.setUiContent('keyboard/KeyboardView');
      if (this.tornDown) {
        if (this.panel === panel) {
          this.panel = undefined;
          await engine.destroyPanel(panel);
        }
        return;
      }
      // A panel has no size until it is given one, and a zero-height panel never appears. The height
      // is the candidate row plus four key rows, or the strip alone where the keys are not drawn; the
      // width is the display width, which the panel reports through its own window rather than being
      // assumed here.
      // vp2px is a UI-context global and an extension ability has no UI context here, so the
      // conversion is done from the display's own density rather than borrowed from the environment.
      const screen: display.Display = display.getDefaultDisplaySync();
      // The same numbers the view lays itself out with; a panel that disagrees clips its bottom row.
      const settings: KeyboardPreferences = KeyboardSession.shared.preferences();
      const heightVp: number = desktop
        ? KeyboardMetrics.candidateHeightVp(settings.candidateLayout, 0,
          settings.candidateShowPreedit, settings.candidateDecorationTopVp,
          KeyboardSession.shared.glossRows(), settings.candidateFontSize,
          settings.candidatePreeditFontSize)
        : KeyboardMetrics.totalHeightVp(settings.rowSpacingTenths, settings.heightAdjustment,
          KeyboardSession.shared.glossRows(), settings.candidateFontSize,
          settings.candidatePreeditFontSize);
      // A soft keyboard is as wide as the screen because it has to be; a candidate window is as wide
      // as its content, and covering the width of a desktop with one would hide the very text the
      // candidates are for.
      const widthPx: number = desktop
        ? Math.round(KeyboardMetrics.CANDIDATE_WINDOW_WIDTH_VP * screen.densityPixels)
        : screen.width;
      this.density = screen.densityPixels;
      this.screenWidth = screen.width;
      this.screenHeight = screen.height;
      await panel.resize(widthPx, Math.round(heightVp * screen.densityPixels));
      if (this.tornDown) {
        if (this.panel === panel) {
          this.panel = undefined;
          await engine.destroyPanel(panel);
        }
        return;
      }
      // The view asks; only the ability holds the panel.
      KeyboardSession.shared.onDismiss = (): void => {
        panel.hide();
      };
      if (desktop) {
        KeyboardSession.shared.onCandidateWidth = (widthVp: number): void => {
          this.candidateWidthVp = widthVp;
        };
        KeyboardSession.shared.onCandidateCount = (count: number): void => {
          this.candidateCount = count;
          if (KeyboardSession.shared.composing()
            && KeyboardSession.shared.openSurface() === DesktopSurface.NONE) {
            this.resizeCandidateWindow(DesktopSurface.NONE).then((): void => {
              this.placeCandidateWindow();
            }).catch((error: Object): void => {
              KeyboardLog.error(`candidate window would not resize: ${describeError(error)}`);
            });
          }
        };
        KeyboardSession.shared.onComposition = (active: boolean): void => {
          // A surface the toolbar opened stays open across a composition; it was asked for, and a
          // window that shut itself the moment a word was typed would be useless for picking an
          // emoji to put in that word.
          if (KeyboardSession.shared.openSurface() !== DesktopSurface.NONE) {
            return;
          }
          if (active) {
            this.candidateAnchor = KeyboardSession.shared.preferences().candidateFollowCursor
              ? undefined : [this.caret[0], this.caret[1], this.caret[2]] as CandidateAnchor;
            this.resizeCandidateWindow(DesktopSurface.NONE);
            this.placeCandidateWindow();
            panel.show();
          } else {
            this.candidateAnchor = undefined;
            this.candidateWidthVp = KeyboardMetrics.CANDIDATE_WINDOW_WIDTH_VP;
            panel.hide();
          }
        };
        KeyboardSession.shared.onSurfaceRequested = (surface: DesktopSurface): void => {
          this.showDesktopSurface(surface);
        };
      } else {
        KeyboardSession.shared.onTouchHeight = (requestedHeightVp: number): void => {
          panel.resize(screen.width, Math.round(requestedHeightVp * this.density))
            .catch((error: Object): void => {
              KeyboardLog.error(`touch keyboard would not resize: ${describeError(error)}`);
            });
        };
      }
      if (desktop) {
        await this.reconcileStatusPanels();
      }
      KeyboardLog.info(`panel ready: ${KeyboardFormFactor.describe()}, `
        + `${desktop ? 'candidate window' : 'soft keyboard'}`
        + `${this.toolbar !== undefined ? ', with toolbar' : ''}`
        + `${this.hud !== undefined ? ', with mode badge' : ''}`);
      // Serve whatever focused while the panel was still being built.
      if (this.pending !== undefined) {
        KeyboardSession.shared.attach(this.pending);
        this.pending = undefined;
        if (!desktop) {
          this.panel.show();
        }
      }
    } catch (error) {
      // The framework allows exactly one soft keyboard panel. If it refused, the session has nothing
      // to draw into, so tear it back down rather than leaving it attached to no surface.
      KeyboardLog.error(`panel creation failed: ${describeError(error)}`);
      this.teardown();
      return;
    }
  }

  onDestroy(): void {
    this.teardown();
  }

  /** Undoes everything onCreate installed. Safe to run twice: a failed onCreate and then onDestroy. */
  private teardown(): void {
    this.statusPanelGeneration += 1;
    this.tornDown = true;
    this.pending = undefined;
    engine.off('inputStart', this.started);
    engine.off('inputStop', this.stopped);
    inputMethodEngine.getKeyboardDelegate().off('selectionChange', this.selectionChanged);
    inputMethodEngine.getKeyboardDelegate().off('textChange', this.textChanged);
    // Before releasing keyEvent, so a queued hot-plug change cannot subscribe it again.
    HarmonyHardwareKeyboards.unwatch();
    if (this.routingKeys) {
      inputMethodEngine.getKeyboardDelegate().off('keyEvent', this.typed);
      this.routingKeys = false;
    }
    this.attachedKeyboards = [];
    if (KeyboardFormFactor.isDesktop()) {
      inputMethodEngine.getKeyboardDelegate().off('cursorContextChange', this.caretMoved);
    }
    KeyboardSession.shared.shutdown();
    // These close over the panel being released.
    KeyboardSession.shared.onTouchHeight = undefined;
    KeyboardSession.shared.onDismiss = undefined;
    KeyboardSession.shared.onCandidateWidth = undefined;
    KeyboardSession.shared.onCandidateCount = undefined;
    KeyboardSession.shared.onComposition = undefined;
    KeyboardSession.shared.onSurfaceRequested = undefined;
    KeyboardSession.shared.onOpenSettings = undefined;
    KeyboardSession.shared.onPreferencesReloaded = undefined;
    this.closeModeBadge();
    this.closeToolbar();
    const panel: inputMethodEngine.Panel | undefined = this.panel;
    this.panel = undefined;
    if (panel !== undefined) {
      engine.destroyPanel(panel).catch((error: Object): void => {
        KeyboardLog.error(`panel would not destroy: ${describeError(error)}`);
      });
    }
  }
}
