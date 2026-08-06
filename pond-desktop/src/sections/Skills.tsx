import { useState, useEffect } from "react";
import { Card, CardContent, Button, Input, Switch } from "@heroui/react";
import { Plus, Trash2, Sparkles, Check } from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, useConfirm } from "../components/shared";
import type { UserSkill } from "../api/types";

export function Skills() {
  const confirm = useConfirm();
  const [skills, setSkills] = useState<UserSkill[]>([]);
  const [loading, setLoading] = useState(true);
  const [name, setName] = useState("");
  const [content, setContent] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function load() {
    setLoading(true);
    api
      .listSkills(true)
      .then(setSkills)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  async function add() {
    if (!name.trim() || !content.trim()) return;
    try {
      await api.addSkill(name.trim(), content.trim());
      setName("");
      setContent("");
      setShowForm(false);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  async function toggle(id: string, currentActive: boolean) {
    try {
      await api.toggleSkill(id, currentActive);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  async function remove(id: string, skillName: string) {
    if (!await confirm(`Delete skill "${skillName}"? This cannot be undone.`, { title: "Delete Skill", confirmLabel: "Delete", destructive: true })) return;
    try {
      await api.removeSkill(id);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="screen screen--skills">
      <PageHeader
        title="Skills"
        action={
          <Button
            color="secondary"
            radius="md"
            onPress={() => setShowForm((v) => !v)}
            startContent={showForm ? undefined : <Plus size={14} />}
          >
            {showForm ? "Cancel" : "Add Skill"}
          </Button>
        }
      />

      {showForm && (
        <Card className="card">
          <CardContent>
            <div className="skills-form">
              <Input
                label="Skill name"
                placeholder="e.g. Code reviewer"
                variant="bordered"
                radius="md"
                value={name}
                onValueChange={setName}
              />
              <textarea
                className="skills-form__textarea"
                placeholder="Describe what this skill should do, when it should activate, and any constraints..."
                value={content}
                onChange={(e) => setContent(e.target.value)}
                rows={4}
              />
              <div className="skills-form__actions">
                <Button
                  variant="light"
                  radius="md"
                  onPress={() => { setShowForm(false); setName(""); setContent(""); }}
                >
                  Cancel
                </Button>
                <Button
                  color="secondary"
                  radius="md"
                  onPress={add}
                  isDisabled={!name.trim() || !content.trim()}
                  startContent={<Check size={14} />}
                >
                  Save skill
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {error && (
        <p className="text-error text-error--sm">{error}</p>
      )}

      {loading ? (
        <p className="muted-12">Loading...</p>
      ) : skills.length === 0 && !showForm ? (
        <Card className="card">
          <CardContent className="card-body--list">
            <div className="empty-state">
              <Sparkles size={20} />
              <span>No skills yet. Add one to teach Pond a new capability.</span>
            </div>
          </CardContent>
        </Card>
      ) : skills.length > 0 ? (
        <Card className="card">
          <CardContent className="card-body--list">
            {skills.map((s) => (
              <div key={s.id} className="skill-row">
                <div className="skill-row__icon">
                  <Sparkles size={16} />
                </div>
                <div className="skill-row__main">
                  <div className="skill-row__name">{s.name}</div>
                  <div className="skill-row__instr">
                    {s.content || <span className="muted">No instructions</span>}
                  </div>
                </div>
                <Switch
                  size="sm"
                  isSelected={s.active}
                  onChange={() => toggle(s.id, s.active)}
                  aria-label={`Enable ${s.name}`}
                >
                  <Switch.Content>
                    <Switch.Control><Switch.Thumb /></Switch.Control>
                  </Switch.Content>
                </Switch>
                <Button
                  isIconOnly
                  size="sm"
                  variant="light"
                  onPress={() => remove(s.id, s.name)}
                  aria-label={`Delete ${s.name}`}
                >
                  <Trash2 size={15} />
                </Button>
              </div>
            ))}
          </CardContent>
        </Card>
      ) : null}
    </div>
  );
}
