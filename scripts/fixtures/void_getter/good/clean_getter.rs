// Self-test input for the void-getter check: a getter whose stored value is
// never void is clean. The `payload` field is declared as a `StoredValue` but
// its constructor default is an integer, and `label` is a plain string, so
// neither can hand the script a raw void.

pub(crate) struct PlayerInst {
    pub(crate) label: String,
    pub(crate) payload: StoredValue,
}

impl Default for PlayerInst {
    fn default() -> Self {
        Self {
            label: String::new(),
            payload: StoredValue::Integer(0),
        }
    }
}

mp_get_string!(player_get_label, PlayerInst, |p: &PlayerInst| p
    .label
    .clone());
mp_get_variant!(
    player_get_payload,
    PlayerInst,
    |p: &PlayerInst, ctx: &CompatCtx| p.payload.to_retained(ctx)
);
