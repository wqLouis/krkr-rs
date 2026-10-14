// Fixture reference media whose domain is documented as a missing seam in the
// port-side plugin module; the check must not flag `gamma`.

#define BASENAME TJS_W("gamma")

class GammaStorage : public iTVPStorageMedia {
public:
    void GetName(ttstr &name) override { name = BASENAME; }
};
