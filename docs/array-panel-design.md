# Array panel — interaction design brief

**Status:** working design brief for the experimental Array branch; not a product-wide policy.
**Scope:** the Array editing experience, its session semantics, and stage controls. Follow
[AGENTS.md](../AGENTS.md), [product principles](product-principles.md), and
[UI guidelines](ui-guidelines.md) for implementation conventions.

## The idea

An Array operation makes a spatial arrangement of ordinary, independent AniCraft
instances or artwork. It interpolates properties *across generated instances* (not
across timeline frames) using draggable property stops. The final result is baked,
not a persistent procedural object. In this first iteration, only a selected
symbol instance is supported.

Preserve the existing `array.create` engine command and its inclusive count
semantics: **10 means 10 total instances, including the original.** Instance 1
repositions the original and retains its identity. The preview is reversible;
Commit records one undo step and selects the result.

## A deliberate mode, not an always-active panel

An Array panel appears in AniCraft's normal dock system. The topmost setting is:

**Array type:** `None` (default), `Linear`, `Grid`, `Radial`, `Along Path`.

Only Linear and Radial are implemented initially. Present Grid and Along Path as
unavailable, rather than pretending they generate anything.

- **None** means no active Array editing transaction. The panel may remain open.
  No stage preview or Array manipulation handles are active.
- Selecting a supported type with an eligible source selection **enters Array mode**.
  Preview generation starts immediately. Show **Commit** and **Cancel**.
- Changing between supported types does **not** exit Array mode, change the
  source selection, discard property stops, or erase geometry settings held for
  the other type. Regenerate from the original snapshot, not from preview copies.
- **Commit** keeps the baked artwork, ends the gesture as one undo step, returns
  the dropdown to None, and clears the parameter configuration. The next Array starts
  with defaults anchored to the selected symbol's current position.
- **Escape**, **Cancel**, or changing the dropdown to **None** abandons the
  preview and restores the pre-entry document and selection without an undo step.
  These exits return the dropdown to None and retain the configuration for recovery.
- A hidden, closed, or backgrounded Array panel must **not silently discard** an
  active session. Make the active operation discoverable; design safe navigation
  and explicit exit. Avoid hijacking Escape from an open child popup or text field:
  normal UI dismissal should take precedence where necessary.
- If other editing actions conflict with the active preview, do not permit them
  to destroy the transaction. Treat array preview as an explicit editing session.
- The mode's geometry and property configuration is **view state** and is not
  itself written into the document or undo stack.

### Remembering versus resetting

Keep the unfinished configuration, including geometry for each array type and property stops,
after Cancel, Escape or None. Commit clears it. Each new session binds to the current eligible
selection. Dim Stage and gizmo visibility carry over across every exit.

Offer a distinct **Reset Settings to Defaults** action. This resets remembered
stops and geometry and is separate from Cancel. Because it can erase elaborate
stop configurations, ask for confirmation before overwriting customized values;
do not reset the document or break the preview transaction. The exact reset
button position can be refined during UI review. Remembered settings across
an app restart are a future enhancement; retention within the running session
is required now.

## Geometry: interactive stage handles

**Linear:** the controlling metaphor is a **barbell**:

- A draggable **Start** circle is the registration point of instance 1.
- A draggable **End** circle is the registration point of the last instance.
- A thin line connects them. Its invisible hit target extends beyond the painted line so the
  bar is easy to grab, including when it is diagonal.
- Drag either circle to update Start/End X/Y, total distance, direction, and
  derived per-copy spacing immediately in the panel.
- Drag the line itself to translate both endpoints together.
- The last instance occupies the end point when count is greater than one.
  For count = 1, only the Start influences the result; handle the End visibly
  but without dividing by zero.
- Start and End are authoritative in the panel and on stage. Coordinates, Direction, Length,
  Spacing X and Spacing Y use the same draggable and typeable fields as Instances.
  Derived edits hold Start fixed and move End. Direction preserves length, Length preserves
  direction, and a spacing component changes only its corresponding axis. At zero length a
  new length extends along +X and Direction becomes available again. Changing count keeps both
  endpoints fixed. With one instance, spacing is unavailable and the other geometry fields
  remain editable.
- The handles and line update the existing live preview. Commit, Cancel, Escape and undo
  follow the same rules as edits to numeric fields.
- Draw a small white circle with a dark outline for each generated instance, including the
  original, at its custom pivot or registration point. The circles are visual indicators in
  both modes. The blue Linear handles remain the draggable controls.
- Dragging is available across the pasteboard. Keep feedback live while reusing stable preview
  artwork and unaffected stage tiles where possible. Measure inside- and outside-stage drags at
  different counts before and after performance changes.

**Radial:** extend the same direct manipulation metaphor:

- Draggable **Centre/Fulcrum** moves the orbit of all copies together.
- Draggable **Start** on the circumference controls radius and starting angle.
- Draggable **Arc End** controls sweep angle (and must distinguish arc direction
  where appropriate). Show the circular arc and tick marks for instances.
- Numeric Centre X/Y, Radius, Start Angle, and Sweep Angle remain synchronized.
- A closed 360° array uses distinct positions, not an overlapping duplicate at
  the start/end seam.
- Keep each instance's *orbit* separate from its own *rotation*.

### Gizmo visibility

Place a compact circle–line–circle toggle beside **Dim Stage**. Use a rounded button with a
visible surface in both states and the theme's active treatment when on. Its tooltip follows
the action, **Hide Array Handles** or **Show Array Handles**. It starts on in both modes. Hiding
it removes the handles, guides and registration markers while live artwork and numeric controls
remain available. Visibility is an editor setting. Radial handles belong to a later increment.

## Dim Stage

**Dim Stage** is a checkbox, **checked by default** during Array mode.
Unrelated artwork is visually subdued and the entire Array is drawn above it, preserving the
preview instances' internal order and their clipping. The stage restores the actual document
stacking when the checkbox is off. Dimming, foreground composition and markers are editor-only
and never change colour effects, document order, or exported artwork.

The artist can turn it off to inspect true colours, alpha, blend, filters, and
overlap against the unmodified surrounding artwork.

## Property stops — reuse the existing gradient interaction, not its colour meaning

Use the existing gradient stop bar's interaction conventions and, ideally, its
shared implementation: add/select/drag/reorder/remove stop handles, keyboard
nudge, editable stop position, and the selected-stop model.

However, the **Array ramp is not a colour gradient**. A stop may eventually
capture transform, opacity, tint/brightness/advanced colour effects, filter
parameters, playback offset for Graphic symbols, stacking depth, and easing.

- Show a **neutral horizontal track** with draggable pentagonal stop handles and a distinct
  selected marker. The track carries positions and selection while Alpha is edited below.
- One rounded container beneath the track holds the selected stop's properties. Its section
  order is **Transform**, **Colour Effect**, **Filters**, **Playback**, and **Stacking Order**.
  Show sections with implemented controls that apply to the source artwork. **Colour Effect ›
  Alpha** is the section available in this increment.
- Preserve independently draggable stops at positions 0..100%. Example:
  positions 0%, 40%, 100% can have Alpha 75%, 100%, 3%; dragging the 40% stop
  changes how quickly that middle value is approached.
- The original source's filter list provides the filter *types and order*.
  Future stops may alter those existing filter values but do not add/delete
  filter types. Give each filter a rounded container within the Filters section, following
  AniCraft's Filters UI. Filter interpolation is a later increment.
- Future multiple-property stops share one set of stop positions; optional
  easing applies between stops.
- Future z-order interpolation can sample numeric depths and then sort copies
  stably, since visual stacking is discrete.
- A radial alpha progression of 100% -> 0% -> 100% can be achieved with three
  stops. Do not impose aesthetic loop-correction rules.

## Future distribution modes (not in this milestone)

- **Grid:** count X and Y, spacing, optional staggering; property progression
  direction can be Horizontal (left->right), Vertical (top->bottom), or Diagonal
  (top-left->bottom-right), with reversal later.
- **Along Path:** distribute evenly by arc length, by equal path parameter
  intervals, or at Bézier anchor **nodes**. Closed paths avoid duplicated
  initial nodes. Path selection and tangent alignment need later UI work.
- **Chaos:** a global intensity slider (0..100%) with property categories
  such as position, rotation, scale and Alpha; deterministic random seed so
  editing stops doesn't reshuffle unrelated positions.
- Source can eventually be multiple selected instances, a group, a drawing
  object, or mergeable raw artwork. Initially limit to one symbol instance,
  with explicit feedback.

## Milestone sequence and acceptance criteria

1. **Editing lifecycle:** dropdown None/Linear/Radial with future options
   disabled; immediate live preview upon entering mode; switching types
   regenerates from the same original, without accumulating copies; Commit
   and Cancel/Escape/None have the distinct effects above. Closing/switching
   dock tabs doesn't silently cancel.
2. **Stop track:** neutral visualization, existing gradient-edit gestures
   and keyboard interactions, Alpha value in a collapsible Colour Effect
   section, multi-stop interpolation. Gradient's original colour editor
   must behave exactly as it did before.
3. **Linear controls and stop editor:** rounded selected-stop container, Linear Start/End/bar,
   synchronized coordinates and derived spacing, and a gizmo visibility toggle beside the
   editor-only **Dim Stage** checkbox.
4. **Interaction polish** covers editable derived geometry, visible registration markers in both
   modes, foreground Dim Stage composition, and measured improvements to live Linear dragging.
5. Later: Radial Centre/Start/Arc End, Grid, Path/node placement, spiral arrays, more stop
   properties, filter interpolation, easing, Chaos,
   multi-selection, optional grouping/merging.

For each milestone run relevant engine and shell tests, Clippy and UI screenshot
checks as prescribed by AGENTS.md. Add lifecycle regression tests for: an
inclusive copy count; switching modes; Escape and None restoring the original
selection/document; configuration retained after Cancel, Escape and None; Commit producing
one undo step, preserving the view options and initializing the next Array from the new source;
no accumulating copies during repeated preview edits; and correct
stop-bar behaviour in both Gradient and Array editors.

For the Linear gizmo, verify both endpoint drags, line translation, numeric fields, fixed
endpoints when count changes, visibility toggling and session exits during stage edits.

**Do not modify AGENTS.md merely to encode this feature's design.** A lasting
architectural choice can be recorded in an ADR following the repository's normal
reservation process if maintainers choose to adopt it.
