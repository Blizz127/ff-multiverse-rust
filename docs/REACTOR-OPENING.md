# Reactor opening survey

The current new-game story starts at moment 1, after the original `md1stin` opening; the
opening is not played. The unregistered `reactor_opening_scene()` is a diagnostic definition,
not a playable scene or preview. It does not change or initialize any account's saved state.

With the owner's original field at moment 0, the director runs one dialogue window, starts
ten measured field actors, initializes several persistent values including Tifa's affection,
and continues into its later field loop. A raw interpreter run reaches a battle termination,
but that does not prove a correct battle trigger: the survey has not staged a known triangle for Cloud when the loop runs `GETAI`. The
run also skips the opening movie, sound, party setup and controls. The safety guard rejects
this outcome. The arrival position is only the field script's Cloud placement, not a verified
player witness position.

The owned Echo-S replacement's director at moment 0 jumps to field 109 before any dialogue;
at moment 1 its opening branch produces one different window with a matching voice file.
The original opening writes moment 1 before entering its position loop; the Echo-S opening
writes moment 2 there. Neither value can simply be substituted for the other in shared saved
progress without reconciling the downstream script guards.
That sampled run likewise continues into a battle termination after an unresolved `GETAI`, and skips
movie, sound, party setup and a heal operation. It also sets additional persistent values. These
runs are diagnostics, not an interpretation of the full scene or proof of equivalent progress.
They must not be substituted directly into the live story. Finishing the opening requires a
bounded scene boundary, movie and sound presentation, correct battle/party handling, a safe
per-player initialization design and a way to distinguish new from existing player progress.
The owner-only live-data decision already requested for story persistence remains applicable;
no reset or automatic migration is implied.

A separate raw-VM run of Echo-S `blackbgh` at moment 0 reached the jump to `md1stin`
after four windows and wrote moment 1 plus `Var[1][10]` bit 0. None of those windows'
first-page voice lookups matched a file in the sampled archive. The run skips `MENU` and
`SPECIAL` and reads the untracked flag before changing it; it is not safe live progression.
These observations are captured in an opt-in owned-data test, not a rendered or audible scene.
