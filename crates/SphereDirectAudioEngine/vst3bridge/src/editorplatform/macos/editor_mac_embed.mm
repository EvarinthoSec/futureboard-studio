// editor_mac_embed.mm — attach a VST3 IPlugView into a host-supplied NSView.
// Compiled as Objective-C++ (.mm) with -fobjc-arc.
//
// The difference from editor_mac.mm, which is the whole point of this file:
// that one *owns* an NSWindow and an NSView and puts the editor in a window of
// its own. This one owns no AppKit object at all. The container NSView belongs
// to the Rust side (SphereUIComponents `plugin_editor_mac_region`), which
// mounts it in the GPUI plug-in editor window and positions it to the shell's
// plug-in region. All this file does is create the IPlugView and attach it
// there.
//
// That split is deliberate and load-bearing. AppKit views are owned by whoever
// put them in the hierarchy, and having two layers both believe they own one is
// how a view gets released out from under a plug-in that is still drawing into
// it. So: the container is borrowed here, never retained, never moved, never
// resized, and never released.
//
//   embed_editor_mac()   → sphere_daux_editor_create_view("NSView")
//                          (which installs MacPluginEditorFrame for resizeView)
//                        → sphere_daux_editor_attach_view(host NSView*)
//   embed_resize_mac()   → IPlugView::onSize, after the host moved its own view
//   embed_detach_mac()   → IPlugView::removed(), container untouched
//
// Detaching is an *editor* lifecycle event: the processor, its component and
// its DSP are not touched here, so closing and reopening an editor never
// interrupts audio.

#include "editor_mac_internal.hpp"

#import <dispatch/dispatch.h>

#include <cstdio>

namespace {

constexpr int kMinEditorDimension = 16;
constexpr int kMaxEditorDimension = 8192;

bool daux_embed_dimensions_sane(int width, int height) {
  return width >= kMinEditorDimension && height >= kMinEditorDimension &&
         width <= kMaxEditorDimension && height <= kMaxEditorDimension;
}

} // namespace

unsigned long long embed_editor_mac(SphereDauxVst3Processor *proc,
                                    void *host_view, int width, int height) {
  if (!proc || !host_view)
    return 0;

  // Every AppKit and VST3 GUI call below must be on the main thread. The
  // callers are GPUI window methods, which already are; this is the guard for
  // the caller added later without noticing.
  if (!NSThread.isMainThread) {
    __block unsigned long long result = 0;
    dispatch_sync(dispatch_get_main_queue(), ^{
      result = embed_editor_mac(proc, host_view, width, height);
    });
    return result;
  }

  NSView *container = (__bridge NSView *)host_view;
  if (![container isKindOfClass:[NSView class]]) {
    std::fprintf(stderr,
                 "[MacPluginEditor] attach refused reason=host_view_not_nsview\n");
    return 0;
  }

  // Already embedded for this instance? Re-attaching would call attached() on a
  // live view, which most editors do not survive. Hand back the same handle and
  // let the host re-sync geometry through embed_resize_mac.
  if (proc->editor_embed_mode && proc->editor_attached &&
      proc->editor_embed_parent == host_view) {
    return sphere_daux_editor_get_handle(proc);
  }

  // A different container for an already-attached editor means the shell
  // rebuilt its region. Take the view out of the old one first — leaving it
  // attached to a view that is about to be released is a use-after-free with
  // extra steps.
  if (proc->editor_attached)
    embed_detach_mac(proc);

  int editor_width = daux_embed_dimensions_sane(width, height) ? width : 820;
  int editor_height = daux_embed_dimensions_sane(width, height) ? height : 560;

  // create_view also installs MacPluginEditorFrame via IPlugView::setFrame,
  // which is what makes plug-in-driven resizeView() requests work, and it
  // checks isPlatformTypeSupported("NSView") before returning success.
  if (!sphere_daux_editor_create_view(proc, "NSView", &editor_width,
                                      &editor_height)) {
    std::fprintf(stderr,
                 "[MacPluginEditor] attach failed reason=create_view_nsview\n");
    return 0;
  }

  const unsigned long long handle = sphere_daux_editor_next_handle();
  std::fprintf(stderr,
               "[MacPluginEditor] attach begin handle=%llu host_view=%p "
               "size=%dx%d\n",
               handle, host_view, editor_width, editor_height);

  if (!sphere_daux_editor_attach_view(proc, host_view, "NSView")) {
    std::fprintf(stderr,
                 "[MacPluginEditor] attach failed reason=attach_view handle=%llu\n",
                 handle);
    return 0;
  }

  // Recorded so detach and geometry updates can find the same container, and
  // so a second open of the same editor is a no-op rather than a re-attach.
  // Stored as a bare pointer on purpose: this file does not own it.
  proc->editor_embed_parent = host_view;
  proc->editor_embed_mode = true;
  proc->editor_handle = handle;

  // Some editors settle on their real size only inside attached(); ask again so
  // the shell reserves what the plug-in actually took rather than what it asked
  // for beforehand.
  int attached_width = editor_width;
  int attached_height = editor_height;
  if (sphere_daux_editor_get_view_size(proc, &attached_width,
                                       &attached_height) &&
      daux_embed_dimensions_sane(attached_width, attached_height)) {
    editor_width = attached_width;
    editor_height = attached_height;
  }
  sphere_daux_editor_set_content_size(proc, editor_width, editor_height);

  std::fprintf(stderr,
               "[MacPluginEditor] attached handle=%llu size=%dx%d resizable=%d\n",
               handle, editor_width, editor_height,
               sphere_daux_editor_can_resize(proc));
  return handle;
}

void embed_resize_mac(SphereDauxVst3Processor *proc, int width, int height) {
  if (!proc || !proc->editor_embed_mode || !proc->editor_attached)
    return;
  if (!daux_embed_dimensions_sane(width, height))
    return;

  if (!NSThread.isMainThread) {
    dispatch_sync(dispatch_get_main_queue(), ^{
      embed_resize_mac(proc, width, height);
    });
    return;
  }

  // The host already moved its own container; this only tells the plug-in what
  // its view is now. Deliberately no AppKit call here — resizing a view this
  // file does not own is exactly the double-ownership the header warns about.
  //
  // The caller is expected to have filtered out unchanged geometry
  // (`HostRegionModel::set_geometry` returns `None` for it): some editors
  // rebuild their entire UI inside onSize, and calling it every layout pass
  // makes them stutter.
  sphere_daux_editor_notify_resize(proc, width, height);
  sphere_daux_editor_set_content_size(proc, width, height);
}

void embed_detach_mac(SphereDauxVst3Processor *proc) {
  if (!proc)
    return;

  if (!NSThread.isMainThread) {
    dispatch_sync(dispatch_get_main_queue(), ^{
      embed_detach_mac(proc);
    });
    return;
  }

  const unsigned long long handle = sphere_daux_editor_get_handle(proc);
  // Releases the IPlugView through IPlugView::removed(). The container view is
  // the host's and is left exactly as it was found — still mounted, still
  // sized, ready for the next open.
  sphere_daux_editor_detach_view(proc);
  proc->editor_embed_parent = nullptr;
  proc->editor_embed_mode = false;
  if (handle)
    std::fprintf(stderr, "[MacPluginEditor] detached handle=%llu\n", handle);
}

int embed_is_attached_mac(SphereDauxVst3Processor *proc) {
  return (proc && proc->editor_embed_mode && proc->editor_attached) ? 1 : 0;
}
