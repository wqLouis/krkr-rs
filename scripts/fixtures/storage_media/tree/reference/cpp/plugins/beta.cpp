// Fixture reference media whose domain is served by the port-side
// `register_media("beta://…")` call; the check must not flag `beta`.

class BetaMedia : public iTVPStorageMedia {
public:
    void GetName(ttstr &name) override { name = TJS_W("beta"); }
};
