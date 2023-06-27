const initialEnv = {};

function parseExpr(program) {
  let current = 0;
  const tokens = program
    .replace(/\(/g, ' ( ')
    .replace(/\)/g, ' ) ')
    .replace(/;.*/g, '') // Remove comments starting with ';'
    .trim()
    .split(/\s+/);

  function parse() {
    if (current >= tokens.length) {
      throw new Error('Unexpected EOF');
    }

    const token = tokens[current++];

    if (token === '(') {
      const expression = [];

      while (tokens[current] !== ')') {
        expression.push(parse());
      }

      current++; // Consume the closing parenthesis
      return expression;
    } else if (token === ')') {
      throw new Error('Unexpected ")"');
    } else {
      return atom(token);
    }
  }

  function atom(token) {
    const num = parseFloat(token);
    return isNaN(num) ? token : num;
  }

  const expressions = [];

  while (current < tokens.length) {
    expressions.push(parse());
  }

  return expressions;
}

const evaluateExpr = expr => {
  const operators = Object.assign({
    "+": (a, b) => a + b,
    "-": (a, b) => a - b,
    "*": (a, b) => a * b,
    "/": (a, b) => a / b,
    "<=": (a, b) => a <= b,
  }, initialEnv);

  const evaluate = (ast, env) => {
    if (Array.isArray(ast)) {
      if (ast[0] === "df") {
        const [, funcName, params, body] = ast;
        env[funcName] = { params, body };
      } else if (ast[0] === "if") {
        const [, condition, trueExpr, falseExpr] = ast;
        const result = evaluate(condition, env);
        const expr = result ? trueExpr : falseExpr;
        return evaluate(expr, env);
      } else if (ast[0] === "lambda") {
        const [, params, body] = ast;
        return (...args) =>
          evaluate(body, { ...env, ...Object.fromEntries(params.map((p, i) => [p, args[i]])) });
      } else {
        let result = evaluate(ast[0], env);

        for (let i = 1; i < ast.length; i++) {
          if (ast[i] === "|") {
            result = evaluate(ast[i + 1], env)(result);
            i++; // Skip the next expression since it has been piped
          } else {
            const arg = evaluate(ast[i], env);
            result = result(arg);
          }
        }

        return result;
      }
    } else if (typeof ast === "string") {
      if (ast in env) {
        return env[ast];
      } else {
        return ast;
      }
    } else {
      return ast;
    }
  };

  const results = [];
  const env = {};

  for (const operator in operators) {
    if (operators.hasOwnProperty(operator)) {
      env[operator] = operators[operator];
    }
  }

  for (const ast of expr) {
    const result = evaluate(ast, env);
    if (ast[0] !== "df") {
      results.push(result);
    }
  }

  return results;
}


/*

(df add (x y) (+ x y))
(df subtract (x y) (- x y))
(df multiply (x y) (* x y))
(df divide (x y) (/ x y))
(df computeInterest (principal rate) (multiply principal rate))
(df computeTotal (principal rate years) (multiply principal (add 1 (computeInterest rate years))))

(define transaction
  (df (principal rate years)
    (subtract (computeTotal principal rate years) principal)
  )
)

(define transactionResult (transaction 1000 0.1 5))
(transactionResult) ; Compute the result of the transaction


function evaluateFinancialSystem() {
  const program = `
    ; Define the account data structure
    (df account (balance)
      (df deposit (amount)
        (df update-balance (balance amount)
          (+ balance amount))
        (update-balance balance amount))
      (df withdraw (amount)
        (if (>= balance amount)
          (df update-balance (balance amount)
            (- balance amount))
          (throw "Insufficient balance"))
        (update-balance balance amount))
      (df get-balance ()
        balance))

    ; Create two accounts
    (df john-account (account balance)
      (account balance))

    (df mary-account (account balance)
      (account balance))

    ; Perform transactions
    (df perform-transactions ()
      ; Deposit to John's account
      (john-account (deposit 100))

      ; Withdraw from John's account
      (john-account (withdraw 50))

      ; Deposit to Mary's account
      (mary-account (deposit 200))

      ; Withdraw from Mary's account (insufficient balance)
      (mary-account (withdraw 300))
    )

    ; Get balances
    (df get-balances ()
      (john-account (get-balance))
      (mary-account (get-balance)))

    ; Execute transactions and retrieve balances
    (perform-transactions)
    (get-balances)
  `;

  const expr = parseExpr(program);
  const results = evaluateExpr(expr);

  console.log(results);
}

evaluateFinancialSystem();


*/