import { useState, useEffect } from "react";
import { Card, CardContent, Button, Input, Switch } from "@heroui/react";
import { Plus, Trash2, Sparkles, Check, Pencil } from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, useConfirm } from "../components/shared";
import type { UserSkill } from "../api/types";

export function Skills() {
  const confirm = useConfirm();
  const [skills, setSkills] = useState<UserSkill[]>([]);
  const [loading, setLoading] = useState(true);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [content, setContent] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [editingSkill, setEditingSkill] = useState<UserSkill | null>(null);
  const [error, setError] = useState<string | null>(null);

  function load() {
    setLoading(true);
    api
      .listSkills(true)
      .then((res) => setSkills(Array.isArray(res) ? res : []))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  function resetForm() {
    setShowForm(false);
    setEditingSkill(null);
    setName("");
    setDescription("");
    setContent("");
  }

  function startEdit(s: UserSkill) {
    setEditingSkill(s);
    setName(s.name);
    setDescription(s.description);
    setContent(s.content);
    setShowForm(true);
  }

  async function save() {
    if (!name.trim() || !description.trim() || !content.trim()) return;
    try {
      if (editingSkill) {
        await api.updateSkill(editingSkill.id, {
          name: name.trim(),
          description: description.trim(),
          content: content.trim(),
        });
      } else {
        await api.addSkill(name.trim(), description.trim(), content.trim());
      }
      resetForm();
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
            onPress={() => (showForm ? resetForm() : setShowForm(true))}
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
              <h3 className="skills-form__title">{editingSkill ? "Edit skill" : "New skill"}</h3>
              <label htmlFor="skill-name" className="ext-form-label">Skill name</label>
              <Input
                id="skill-name"
                placeholder="e.g. Task Reminder"
                variant="bordered"
                radius="md"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
              <label htmlFor="skill-description" className="ext-form-label">Description</label>
              <Input
                id="skill-description"
                placeholder='When should this activate? e.g. "Creates reminders when asked to be reminded of something"'
                variant="bordered"
                radius="md"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
              />
              <label htmlFor="skill-content" className="ext-form-label">Instructions</label>
              <textarea
                id="skill-content"
                className="skills-form__textarea"
                placeholder="The full instructions Pond follows once this skill activates..."
                value={content}
                onChange={(e) => setContent(e.target.value)}
                rows={4}
              />
              <div className="skills-form__actions">
                <Button
                  variant="light"
                  radius="md"
                  onPress={resetForm}
                >
                  Cancel
                </Button>
                <Button
                  color="secondary"
                  radius="md"
                  onPress={save}
                  isDisabled={!name.trim() || !description.trim() || !content.trim()}
                  startContent={<Check size={14} />}
                >
                  {editingSkill ? "Update skill" : "Save skill"}
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
                    {s.description || <span className="muted">No description</span>}
                  </div>
                </div>
                <Switch
                  size="sm"
                  color="secondary"
                  isSelected={s.active}
                  onValueChange={() => toggle(s.id, s.active)}
                  aria-label={`Enable ${s.name}`}
                />
                <Button
                  isIconOnly
                  size="sm"
                  variant="light"
                  onPress={() => startEdit(s)}
                  aria-label={`Edit ${s.name}`}
                >
                  <Pencil size={14} />
                </Button>
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
