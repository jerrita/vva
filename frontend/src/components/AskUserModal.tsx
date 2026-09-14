import { useEffect, useState } from 'react'
import type { UserAnswer, UserQuestionRequest } from '../types'

export function AskUserModal({
  request,
  onSubmit,
  onCancel,
}: {
  request: UserQuestionRequest
  onSubmit: (answers: UserAnswer[]) => void
  onCancel: () => void
}) {
  const [questionIndex, setQuestionIndex] = useState(0)
  const [selected, setSelected] = useState<string[][]>(() => request.questions.map(() => []))
  const [other, setOther] = useState<string[]>(() => request.questions.map(() => ''))
  const question = request.questions[questionIndex]

  useEffect(() => {
    setQuestionIndex(0)
    setSelected(request.questions.map(() => []))
    setOther(request.questions.map(() => ''))
  }, [request.id, request.questions])

  if (!question) return null

  const currentSelected = selected[questionIndex] ?? []
  const toggle = (label: string) => {
    setSelected(previous => {
      const next = previous.map(values => [...values])
      const values = next[questionIndex] ?? []
      if (question.multi_select) {
        next[questionIndex] = values.includes(label)
          ? values.filter(value => value !== label)
          : [...values, label]
      } else {
        next[questionIndex] = [label]
      }
      return next
    })
  }

  const buildAnswers = (): UserAnswer[] =>
    request.questions.map((item, index) => {
      const extra = other[index]?.trim()
      const values = extra && !item.multi_select ? [extra] : [...(selected[index] ?? [])]
      if (extra && item.multi_select) values.push(extra)
      return { question: item.question, answers: values }
    })

  const advance = () => {
    if (currentSelected.length === 0 && !other[questionIndex]?.trim()) return
    if (questionIndex + 1 < request.questions.length) {
      setQuestionIndex(index => index + 1)
    } else {
      onSubmit(buildAnswers())
    }
  }

  return (
    <div className="modal-overlay ask-user-overlay">
      <section className="modal ask-user-modal" role="dialog" aria-modal="true" aria-labelledby="ask-user-title">
        <div className="ask-user-head">
          <div>
            <div className="modal-kicker">
              {question.header || 'USER INPUT'} · {question.multi_select ? '多选' : '单选'}
            </div>
            <h2 id="ask-user-title">需要你的决定</h2>
          </div>
          <span className="ask-user-progress">
            {questionIndex + 1} / {request.questions.length}
          </span>
        </div>
        <p className="ask-user-question">{question.question}</p>
        <div className="ask-user-options">
          {question.options.map(option => {
            const active = currentSelected.includes(option.label)
            return (
              <button
                key={option.label}
                type="button"
                className={`ask-user-option${active ? ' selected' : ''}`}
                onClick={() => toggle(option.label)}
              >
                <span className="ask-user-check">{active ? '✓' : ''}</span>
                <span>
                  <strong>{option.label}</strong>
                  <small>{option.description}</small>
                </span>
              </button>
            )
          })}
        </div>
        <label className="ask-user-other">
          <span>其他回答</span>
          <input
            value={other[questionIndex] ?? ''}
            onChange={event => {
              const value = event.target.value
              setOther(previous => previous.map((item, index) => (index === questionIndex ? value : item)))
            }}
            placeholder="输入自定义回答（可选）"
            onKeyDown={event => {
              if (event.key === 'Enter') {
                event.preventDefault()
                advance()
              }
            }}
          />
        </label>
        <div className="ask-user-foot">
          <button type="button" className="ghost-button" onClick={onCancel}>取消</button>
          <button
            type="button"
            className="primary-button"
            disabled={currentSelected.length === 0 && !other[questionIndex]?.trim()}
            onClick={advance}
          >
            {questionIndex + 1 < request.questions.length ? '下一题' : '提交回答'}
          </button>
        </div>
      </section>
    </div>
  )
}
