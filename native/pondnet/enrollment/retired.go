package enrollment

import (
	"log/slog"
	"time"
)

// retiredCapacity bounds the retired records one household holds at a time. A
// replacement past it is refused with retired_capacity rather than growing the store
// without limit; expireRetired is what keeps a household below it.
const retiredCapacity = 256

// retiredGrace is how long a retired identity must stay absent from the coordinator
// before its record is dropped. A registration the coordinator was still completing
// when the record was last checked shows up well inside it, resets the clock, and is
// deleted like any other late registration.
const retiredGrace = 24 * time.Hour

// expireRetired drops the retired records that no longer protect anything.
//
// A replacement retires the old record so that Reconcile can still delete a node
// registered late under the old identity. That job is done once the identity is
// gone from the coordinator and stays gone, so a record is dropped when every
// inventory read for at least retiredGrace has shown no node holding it under its
// household's user. The inventory is the one Reconcile read before deleting
// anything, so a node it deleted in this pass still counts as present: absence is
// only ever what a read showed, never what a deletion is assumed to have done.
//
// Dropping a record also ends its identity_retired refusal, so the same machine key
// could be enrolled again. That is deliberate, and no list of expired keys is kept.
// The key is an old phone's, it is held by no coordinator node, and enrolling it
// still needs a fresh approval signed by the household's own Pond for that exact
// key, which is all any new phone needs; the refusal never stood between a
// household and its own approval. A key a node still holds is refused anyway, as
// identity_already_registered.
func (s *Service) expireRetired(inventory []Registered) error {
	type machineHolder struct{ user, machineKey string }
	type nodeHolder struct{ user, id, key string }
	machines, nodes := map[machineHolder]bool{}, map[nodeHolder]bool{}
	for _, node := range inventory {
		machines[machineHolder{node.UserID, node.MachineKey}] = true
		nodes[nodeHolder{node.UserID, node.ID, node.Key}] = true
	}
	// Held by a node carrying the record's machine key or by its exact node, whichever
	// the record carries; one carrying both needs both gone. This is at least as broad
	// as what deleteOwnedFrom deletes, so a node of the household it leaves in place
	// keeps the record.
	held := func(device Device, user string) bool {
		return (device.MachineKey != "" && machines[machineHolder{user, device.MachineKey}]) ||
			(device.NodeID != "" && nodes[nodeHolder{user, device.NodeID, device.Key}])
	}
	now := s.Now().Unix()
	grace := int64(retiredGrace / time.Second)
	changed := false
	for household, retired := range s.Store.value.Retired {
		user := s.Store.value.Households[household].UserID
		kept := retired[:0]
		for _, device := range retired {
			switch {
			case held(device, user):
				changed = changed || device.AbsentSince != 0
				device.AbsentSince = 0
			case device.AbsentSince == 0:
				device.AbsentSince, changed = now, true
			case now-device.AbsentSince >= grace:
				changed = true
				continue
			}
			kept = append(kept, device)
		}
		if expired := len(retired) - len(kept); expired > 0 {
			slog.Info("retired enrollment records expired", "household", household, "expired", expired, "remaining", len(kept))
		}
		if len(kept) == 0 {
			delete(s.Store.value.Retired, household)
		} else {
			s.Store.value.Retired[household] = kept
		}
	}
	if !changed {
		return nil
	}
	return s.Store.save()
}
