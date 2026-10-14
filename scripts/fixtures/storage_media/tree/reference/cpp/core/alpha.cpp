// Fixture reference media with no port serving path and no documented no-op:
// the `storage-media` check must flag `alpha`.

class AlphaMedia : public iTVPStorageMedia {
public:
    void GetName(ttstr &name) override { name = TJS_W("alpha"); }
};
