#pragma once

#import <Cocoa/Cocoa.h>

#include "../../../include/sphere_daux_editor_bridge.h"
#include "../../../include/sphere_daux_editor_chrome.h"
#include "../../../include/sphere_daux_editor_shell_mac.h"

NSColor *daux_bg_color(void);

/// Resize the NSWindow content while preserving its top-left screen position.
/// `notify_plugin` forwards the agreed content size through IPlugView::onSize.
void daux_resize_editor_content(SphereDauxVst3Processor *proc, int width,
                                int height, bool notify_plugin,
                                const char *reason);

void close_editor_mac(SphereDauxVst3Processor *proc);

// ── GPUI-embedded editor (editor_mac_embed.mm) ──────────────────────────────
//
// Attach the plug-in's IPlugView into an NSView the *host* owns, instead of
// into an NSWindow this bridge creates. `host_view` is borrowed for the life of
// the attachment and is never retained, moved, resized or released here.

/// Returns an opaque non-zero handle on success, 0 on failure.
unsigned long long embed_editor_mac(SphereDauxVst3Processor *proc,
                                    void *host_view, int width, int height);

/// Tell the plug-in its view changed size, after the host moved its container.
/// Does not touch AppKit: the container is not ours to resize.
void embed_resize_mac(SphereDauxVst3Processor *proc, int width, int height);

/// IPlugView::removed(). Leaves the host's container mounted and sized, and
/// never touches the processor — closing an editor must not interrupt audio.
void embed_detach_mac(SphereDauxVst3Processor *proc);

int embed_is_attached_mac(SphereDauxVst3Processor *proc);

/// Receives close-button clicks from the NSWindow and delegates them to the
/// processor's close path so IPlugView::removed() is called correctly.
@interface DauxEditorWindowDelegate : NSObject <NSWindowDelegate>
@property(nonatomic, assign) SphereDauxVst3Processor *processor;
@property(nonatomic, assign) BOOL applyingHostResize;
@end
