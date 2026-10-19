// Self-test input for the void-getter check: the raw passthrough must flag.
//
// The getter hands the stored value straight to the script, while the
// reference property reads back an object even before the first assignment.
// Reference: `reference/cpp/plugins/motionplayer/Player.h:99-100`.

pub(crate) struct PlayerInst {
    pub(crate) badge: StoredValue,
}

impl Default for PlayerInst {
    fn default() -> Self {
        Self {
            badge: StoredValue::Void,
        }
    }
}

mp_get_variant!(
    player_get_badge,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.badge.to_retained(ctx)
);
